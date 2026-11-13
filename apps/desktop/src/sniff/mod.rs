//! 嗅探页：左边是网页（后退、前进、刷新、地址栏），右边列出这个页面里找到的视频流。点某条流的「探测」时带上
//! Referer、网页的 User-Agent 与浏览器在这个请求里也会带的 Cookie（见 [`request::headers_for`]），交给新建页。页面的
//! 地址与标题由网页引擎报；脚本发回的只有找到的流与请求数，按不可信数据校验、限量。网页要开的新窗口不开，列出来
//! 由用户决定是否在这里打开。

mod request;
mod script;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Div, Entity, EventEmitter, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use hs_m3u8_app::title_of;
use hs_m3u8_core::Url;
use hs_m3u8_webview::WebView;
use tokio::sync::mpsc;

use self::script::Report;
use crate::links::{NOT_HTTP, http_url};
use crate::new_task::Link;
use crate::theme;
use crate::ui::{self, CAPTION_LINE, GAP, TEXT_LINE, Tone};
use crate::wait::within;

/// 右边一栏的宽度。
const ASIDE_WIDTH: f32 = 380.;
/// 窗口窄时右边一栏最多收到的宽度。
pub const ASIDE_MIN_WIDTH: f32 = 300.;
/// 网页一栏最窄多宽：常见网页的移动版布局从这里起。
pub const PAGE_MIN_WIDTH: f32 = 480.;

/// 一个页面里最多列出多少条流：网页能报任意多条，再多的只计数。
const MAX_FOUND: usize = 200;
/// 最多记多少个框架的请求数：网页能造任意多个框架编号，再多的不记。
const MAX_FRAMES: usize = 64;
/// 网页发来、还没处理的消息最多积多少条：网页发得比处理得快时，再来的丢掉并计数。
const QUEUE: usize = 256;
/// 等网页引擎回出 Cookie 最多多久：引擎卡住时不让探测一直挂着。
const COOKIE_TIMEOUT: Duration = Duration::from_secs(5);

/// 嗅探页发给外面的事件。
pub enum SniffEvent {
    /// 探测这个流
    Probe(Link),
    Dismissed,
}

/// 网页引擎报的页面加载，按发生的先后处理，不丢（只在网页真的导航时发生）。
enum Loading {
    /// 开始加载这个地址：之前找到的都属于上一个页面
    Started(String),
    /// 加载完了这个地址
    Finished(String),
}

/// 网页能任意多发的：脚本的消息、标题的变化与要开的新窗口，积多了丢。
enum Incoming {
    /// 脚本发回的消息与发它的框架的地址（网页引擎给的，见 [`hs_m3u8_webview::ipc_frame`]）
    Message {
        body: String,
        frame: Option<String>,
    },
    Title(String),
    /// 网页要在新窗口里打开的地址（链接的 target=_blank、window.open）
    Popup(String),
}

/// 找到的一条流。
#[derive(Clone)]
struct Found {
    url: Url,
    /// 报它的框架的地址，网页引擎给的；认不出是哪个框架（Linux）时为 None
    frame: Option<Url>,
    /// 报它的框架的 origin，脚本自报的；不是 http(s) 的为 None
    origin: Option<Url>,
    /// 网页的 User-Agent
    agent: String,
}

pub struct SniffView {
    /// 建不出网页时为原因
    webview: Result<Entity<WebView>, String>,
    address: Entity<InputState>,
    /// 网页的标题；还没有时为 None
    title: Option<String>,
    found: Vec<Found>,
    /// 列表满了之后又找到的条数
    overflow: usize,
    /// 各个框架（脚本取的编号）发出的请求数
    requests: HashMap<String, u32>,
    /// 网页发来、积压满了丢掉的消息数，从打开嗅探页起算
    dropped: Rc<Cell<u64>>,
    /// 最近一次操作没成的原因（地址不是链接、读不出 Cookie 等）
    error: Option<String>,
    /// 最近一次探测没带上的、放不进请求头的 Cookie 个数
    skipped: usize,
    /// 网页最近一次要在新窗口里打开的地址
    popup: Option<Url>,
    /// 正在读 Cookie、准备探测的那条流
    reading: Option<Url>,
    /// 网页换了地址、还没写进地址栏的（写地址栏要窗口，下一次画时写）
    navigated: Option<String>,
    _loading: Task<()>,
    _incoming: Task<()>,
    _reading: Task<()>,
    _typed: Subscription,
}

impl EventEmitter<SniffEvent> for SniffView {}

/// 把网页发来的交给处理它的任务：消息太长、积压满了时丢掉并计数，标题先截短；嗅探页关了时什么也不做。
fn offer(sender: &mpsc::Sender<Incoming>, dropped: &Cell<u64>, incoming: Incoming) {
    let incoming = match incoming {
        Incoming::Message { body, .. } if body.len() > script::MAX_MESSAGE => {
            dropped.set(dropped.get() + 1);
            return;
        }
        Incoming::Title(title) => Incoming::Title(script::clip(&title)),
        incoming => incoming,
    };
    match sender.try_send(incoming) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
        Err(mpsc::error::TrySendError::Full(_)) => dropped.set(dropped.get() + 1),
    }
}

impl SniffView {
    pub fn new(url: Url, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (loading, mut loadings) = mpsc::unbounded_channel::<Loading>();
        let (incoming, mut incomings) = mpsc::channel::<Incoming>(QUEUE);
        let dropped = Rc::new(Cell::new(0));
        let builder = wry::WebViewBuilder::new()
            // 播放器常在嵌入的框架里：能经引擎直接发回的平台每一层框架都注入（见 script::SCRIPT）
            .with_initialization_script_for_main_only(script::SCRIPT, cfg!(target_os = "windows"))
            .with_ipc_handler({
                let (incoming, dropped) = (incoming.clone(), dropped.clone());
                move |request| {
                    let frame = hs_m3u8_webview::ipc_frame(&request);
                    let body = request.into_body();
                    offer(&incoming, &dropped, Incoming::Message { body, frame });
                }
            })
            .with_document_title_changed_handler({
                let (incoming, dropped) = (incoming.clone(), dropped.clone());
                move |title| offer(&incoming, &dropped, Incoming::Title(title))
            })
            .with_new_window_req_handler({
                let dropped = dropped.clone();
                move |url, _| {
                    offer(&incoming, &dropped, Incoming::Popup(url));
                    wry::NewWindowResponse::Deny
                }
            })
            .with_on_page_load_handler(move |event, url| {
                // 嗅探页关了时没有人收
                let _closed = loading.send(match event {
                    wry::PageLoadEvent::Started => Loading::Started(url),
                    wry::PageLoadEvent::Finished => Loading::Finished(url),
                });
            })
            .with_url(url.as_str());
        let webview = WebView::build(builder, window, cx)
            .map(|webview| cx.new(|_| webview))
            .map_err(|error| format!("建不出网页：{error}"));
        let loading_task = cx.spawn(async move |this, cx| {
            while let Some(event) = loadings.recv().await {
                if this
                    .update(cx, |this, cx| this.on_loading(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let incoming_task = cx.spawn(async move |this, cx| {
            while let Some(incoming) = incomings.recv().await {
                if this
                    .update(cx, |this, cx| this.on_incoming(incoming, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let address = cx.new(|cx| InputState::new(window, cx));
        address.update(cx, |input, cx| input.set_value(url.to_string(), window, cx));
        let typed = cx.subscribe_in(
            &address,
            window,
            |this: &mut Self, input, event: &InputEvent, _, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let text = input.read(cx).value().to_string();
                    this.open(&text, cx);
                }
                InputEvent::Change | InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        SniffView {
            webview,
            address,
            title: None,
            found: Vec::new(),
            overflow: 0,
            requests: HashMap::new(),
            dropped,
            error: None,
            skipped: 0,
            popup: None,
            reading: None,
            navigated: None,
            _loading: loading_task,
            _incoming: incoming_task,
            _reading: Task::ready(()),
            _typed: typed,
        }
    }

    fn on_loading(&mut self, event: Loading, cx: &mut Context<Self>) {
        match event {
            Loading::Started(url) => {
                self.found.clear();
                self.overflow = 0;
                self.requests.clear();
                self.title = None;
                self.error = None;
                self.skipped = 0;
                self.popup = None;
                self.stop_reading();
                self.navigated = Some(url);
            }
            Loading::Finished(url) => self.navigated = Some(url),
        }
        cx.notify();
    }

    /// 不再等正在读的 Cookie：读完也不探测。
    fn stop_reading(&mut self) {
        self.reading = None;
        self._reading = Task::ready(());
    }

    fn on_incoming(&mut self, incoming: Incoming, cx: &mut Context<Self>) {
        match incoming {
            // 页面发来的按不可信数据校验，认不出的丢掉
            Incoming::Message { body, frame } => match script::parse(&body) {
                Some(Report::Stream { url, origin, agent }) => {
                    if self.found.iter().any(|found| found.url == url) {
                        return;
                    }
                    if self.found.len() < MAX_FOUND {
                        let frame = frame.as_deref().and_then(http_url);
                        self.found.push(Found {
                            url,
                            frame,
                            origin,
                            agent,
                        });
                    } else {
                        self.overflow += 1;
                    }
                }
                Some(Report::Requests { frame, count }) => {
                    if self.requests.len() < MAX_FRAMES || self.requests.contains_key(&frame) {
                        self.requests.insert(frame, count);
                    }
                }
                None => return,
            },
            // 单页应用换页时地址变了而不重新加载，常伴着改标题：顺带读一次地址
            Incoming::Title(title) => {
                self.title = (!title.is_empty()).then_some(title);
                if let Some(webview) = self.webview() {
                    match webview.read(cx).raw().url() {
                        Ok(url) => self.navigated = Some(url),
                        Err(error) => self.error = Some(format!("读不出网页的地址：{error}")),
                    }
                }
            }
            // 不是 http(s) 的（about:blank 之类）没什么可开
            Incoming::Popup(url) => self.popup = http_url(&url),
        }
        cx.notify();
    }

    /// 网页的标题；还没有时为 None。
    pub fn page_title(&self) -> Option<String> {
        self.title.clone()
    }

    fn webview(&self) -> Option<&Entity<WebView>> {
        self.webview.as_ref().ok()
    }

    /// 在网页里打开 `text`（地址栏里输入的，或网页要在新窗口里开、用户点了在这里打开的）。
    fn open(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(url) = http_url(text) else {
            self.error = Some(NOT_HTTP.to_owned());
            cx.notify();
            return;
        };
        if let Some(webview) = self.webview() {
            self.error = webview
                .read(cx)
                .raw()
                .load_url(url.as_str())
                .err()
                .map(|error| format!("打不开：{error}"));
        }
        cx.notify();
    }

    /// 后退、前进或刷新：`act` 调网页的那个操作，`what` 为没做成时的说法。
    fn navigate(
        &mut self,
        act: fn(&wry::WebView) -> wry::Result<()>,
        what: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(webview) = self.webview() {
            self.error = act(webview.read(cx).raw())
                .err()
                .map(|error| format!("{what}：{error}"));
        }
        cx.notify();
    }

    /// 读出网页引擎里发往流 `url` 的 Cookie，配好请求头后交给新建页；读的期间不能再点探测。列表里已没有这条流（网页
    /// 刚换了页）时什么也不做。
    fn probe(&mut self, url: &Url, cx: &mut Context<Self>) {
        if self.reading.is_some() {
            return;
        }
        let Some(webview) = self.webview() else {
            return;
        };
        let Some(found) = self.found.iter().find(|found| &found.url == url).cloned() else {
            return;
        };
        let raw = webview.read(cx).raw();
        let top = match raw.url() {
            Ok(top) => http_url(&top),
            Err(error) => {
                self.error = Some(format!("读不出网页的地址：{error}"));
                cx.notify();
                return;
            }
        };
        let asking = hs_m3u8_webview::cookies::cookies_for(raw, &found.url);
        let title = self.title.clone();
        self.reading = Some(found.url.clone());
        self.error = None;
        self._reading = cx.spawn(async move |this, cx| {
            let asking = std::pin::pin!(asking);
            let answer = within(asking, cx.background_executor().timer(COOKIE_TIMEOUT))
                .await
                .unwrap_or_else(|| {
                    Err(format!(
                        "网页引擎 {} 秒没有回出 Cookie",
                        COOKIE_TIMEOUT.as_secs()
                    ))
                });
            // 读的期间嗅探页关了时不再探测
            let _closed = this.update(cx, |this, cx| {
                this.reading = None;
                let made = answer.and_then(|cookies| {
                    request::headers_for(&request::Found {
                        stream: &found.url,
                        frame: found.frame.as_ref(),
                        origin: found.origin.as_ref(),
                        agent: &found.agent,
                        top: top.as_ref(),
                        cookies,
                    })
                });
                match made {
                    Ok(made) => {
                        this.skipped = made.skipped;
                        cx.emit(SniffEvent::Probe(Link {
                            url: found.url,
                            title,
                            headers: made.headers,
                        }));
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    /// 嗅探页不显示了：藏起网页（原生网页不随 GPUI 的画面消失）、暂停里面的视频与音频，不再等正在读的 Cookie。
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.stop_reading();
        let Some(webview) = self.webview() else {
            return;
        };
        let webview = webview.read(cx);
        let hidden = webview
            .hide()
            .map_err(|error| error.to_string())
            .and_then(|()| webview.suspend_media(true));
        if let Err(error) = hidden {
            self.error = Some(format!("网页没能藏起来或停下播放：{error}"));
        }
        cx.notify();
    }

    /// 嗅探页又显示了：恢复播放（网页随下一次画重新摆出来）。
    pub fn reveal(&mut self, cx: &mut Context<Self>) {
        let Some(webview) = self.webview() else {
            return;
        };
        if let Err(error) = webview.read(cx).suspend_media(false) {
            self.error = Some(format!("网页没能恢复播放：{error}"));
            cx.notify();
        }
    }
}

impl Render for SniffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 网页换了地址：写进地址栏，正在输入时不打断
        if let Some(text) = self.navigated.take()
            && !self.address.read(cx).focus_handle(cx).is_focused(window)
        {
            self.address
                .update(cx, |input, cx| input.set_value(text, window, cx));
        }
        div()
            .flex_1()
            .flex()
            .gap(px(2. * GAP))
            .pb(px(ui::BOTTOM))
            .child(self.render_page(cx))
            .child(self.render_found(cx))
    }
}

impl SniffView {
    /// 左边：工具栏与网页。
    fn render_page(&self, cx: &mut Context<Self>) -> Div {
        let nav = |id: &'static str,
                   label: &'static str,
                   act: fn(&wry::WebView) -> wry::Result<()>,
                   what: &'static str,
                   cx: &mut Context<Self>| {
            ui::button(
                id,
                label,
                Tone::Ghost,
                cx.listener(move |this, _, _, cx| this.navigate(act, what, cx)),
            )
        };
        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(12.))
            .py(px(10.))
            .border_b_1()
            .border_color(theme::line())
            .child(nav("back", "‹", wry::WebView::go_back, "没能后退", cx))
            .child(nav(
                "forward",
                "›",
                wry::WebView::go_forward,
                "没能前进",
                cx,
            ))
            .child(nav("reload", "↻", wry::WebView::reload, "没能刷新", cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(36.))
                    .ml(px(6.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(10.))
                    .bg(theme::raised())
                    .text_size(px(13.))
                    .child(Input::new(&self.address).appearance(false)),
            )
            .child(ui::button(
                "close-sniff",
                "关闭",
                Tone::Ghost,
                cx.listener(|_, _, _, cx| cx.emit(SniffEvent::Dismissed)),
            ));
        let page = match &self.webview {
            Ok(webview) => div().flex_1().min_h_0().child(webview.clone()),
            Err(reason) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .child(ui::error_text(reason.clone())),
        };
        let trouble = self
            .webview()
            .and_then(|webview| webview.read(cx).trouble());
        div()
            .flex_1()
            .min_w(px(PAGE_MIN_WIDTH))
            .flex()
            .flex_col()
            // 原生网页盖住下边，圆角只留上边
            .rounded_t(px(18.))
            .border_1()
            .border_color(theme::line())
            .bg(theme::disc())
            .child(toolbar)
            .child(page)
            .children(trouble.map(|reason| div().p(px(10.)).child(ui::error_text(reason))))
    }

    /// 右边：找到的流、监听的请求数与说明。
    fn render_found(&self, cx: &mut Context<Self>) -> Div {
        let requests: u64 = self.requests.values().map(|&count| u64::from(count)).sum();
        let rows = self.found.iter().enumerate().map(|(index, found)| {
            let reading = self.reading.as_ref() == Some(&found.url);
            div()
                .flex()
                .items_center()
                .gap(px(14.))
                .p(px(12.))
                .rounded(px(14.))
                .border_1()
                .border_color(theme::edge())
                .bg(theme::panel())
                .child(ui::rings(RINGS, 4., 18.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(title_of(&found.url)),
                        )
                        .child(ui::num(
                            div()
                                .text_size(px(12.))
                                .line_height(px(CAPTION_LINE))
                                .text_color(theme::muted())
                                .truncate()
                                .child(SharedString::from(meta(&found.url))),
                        )),
                )
                .child(if reading {
                    ui::caption("读 Cookie…").into_any_element()
                } else {
                    let url = found.url.clone();
                    ui::button(
                        ("probe-found", index),
                        "探测",
                        if index == 0 { Tone::Glow } else { Tone::Plain },
                        cx.listener(move |this, _, _, cx| this.probe(&url, cx)),
                    )
                    .into_any_element()
                })
        });
        let empty = self.found.is_empty().then(|| {
            div()
                .text_size(px(13.))
                .line_height(px(TEXT_LINE))
                .text_color(theme::muted())
                .child("还没找到。在网页里点播放，播放器拉取的 HLS 播放列表会出现在这里。")
        });
        let mut notes = Vec::new();
        if self.overflow > 0 {
            notes.push(format!(
                "只列出前 {MAX_FOUND} 条，另有 {} 条没列出",
                self.overflow
            ));
        }
        let dropped = self.dropped.get();
        if dropped > 0 {
            notes.push(format!(
                "网页发来的消息太多，丢了 {dropped} 条，可能有流没列出"
            ));
        }
        if self.skipped > 0 {
            notes.push(format!(
                "上次探测没带 {} 个 Cookie：没有名字或值里有控制字符，请求头放不下",
                self.skipped
            ));
        }
        let popup = self.popup.clone().map(|url| {
            let target = url.clone();
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(ui::caption(format!("网页要开新窗口：{}", meta(&url)))),
                )
                .child(ui::button(
                    "open-popup",
                    "在这里打开",
                    Tone::Plain,
                    cx.listener(move |this, _, _, cx| {
                        this.popup = None;
                        this.open(target.as_str(), cx);
                    }),
                ))
        });
        div()
            .flex_basis(px(ASIDE_WIDTH))
            .flex_shrink(1.)
            .min_w(px(ASIDE_MIN_WIDTH))
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::BOLD)
                            .child("这个页面里的视频流"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(ui::dot(theme::cyan(), 7.))
                            .child(ui::num(ui::caption(format!("监听中 · {requests} 个请求")))),
                    ),
            )
            .children(self.title.clone().map(ui::caption))
            .child(
                div()
                    .id("found")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .children(rows)
                    .children(empty),
            )
            .children(popup)
            .children(notes.into_iter().map(ui::caption))
            .children(self.error.clone().map(ui::error_text))
            .child(ui::caption(
                "探测时带上流所在页面的网站作 Referer、网页的 User-Agent，与浏览器在这个请求里也会带的 Cookie。",
            ))
    }
}

/// 流的说明：主机与路径；带查询参数的标出（同一个地址常带不同的令牌，只看主机与路径分不出来）。
fn meta(url: &Url) -> String {
    let mut text = format!("{}{}", url.host_str().unwrap_or_default(), url.path());
    if url.query().is_some() {
        text.push_str(" · 带参数");
    }
    text
}

/// 流前面的小同心环（见 [`ui::rings`]）的边长。
const RINGS: f32 = 44.;
