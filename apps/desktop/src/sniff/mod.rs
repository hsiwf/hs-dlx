//! 网页里的视频流：网页在后台加载、不显示，主体列出找到的视频流，每条在后台探测好（几档清晰度、时长或直播），用户
//! 挑一条下载。要登录、认证或手动点播放时可以打开网页（左边网页、右边列表），弄完收起。
//!
//! 探测时带上 Referer、网页的 User-Agent 与浏览器在这个请求里也会带的 Cookie（见 [`request::headers_for`]）。页面的
//! 地址与标题由网页引擎报；脚本发回的只有找到的流与请求数，按不可信数据校验、限量。网页要开的新窗口不开，列出来由
//! 用户决定是否在这里打开。后台的网页不自动播放（没有用户点过的媒体不出声）。

mod request;
mod script;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, Entity, EventEmitter, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use hs_m3u8_app::{Failure, Service, title_of};
use hs_m3u8_core::{Source, Url};
use hs_m3u8_webview::WebView;
use tokio::sync::mpsc;

use self::script::Report;
use crate::links::{NOT_HTTP, http_url};
use crate::new_task::{self, Link};
use crate::theme;
use crate::ui::{self, CAPTION_LINE, GAP, TEXT_LINE, Tone};
use crate::wait::within;

/// 打开网页时右边一栏的宽度。
const ASIDE_WIDTH: f32 = 380.;
/// 窗口窄时右边一栏最多收到的宽度。
pub const ASIDE_MIN_WIDTH: f32 = 300.;
/// 网页一栏最窄多宽：常见网页的移动版布局从这里起。
pub const PAGE_MIN_WIDTH: f32 = 480.;
/// 不显示网页时列表的最大宽度。
const LIST_MAX_WIDTH: f32 = 760.;

/// 一个页面里最多列出多少条流：网页能报任意多条，再多的只计数。
const MAX_FOUND: usize = 200;
/// 最多记多少个框架的请求数：网页能造任意多个框架编号，再多的不记。
const MAX_FRAMES: usize = 64;
/// 网页发来、还没处理的消息最多积多少条：网页发得比处理得快时，再来的丢掉并计数。
const QUEUE: usize = 256;
/// 等网页引擎回出 Cookie 最多多久：引擎卡住时不让探测一直挂着。
const COOKIE_TIMEOUT: Duration = Duration::from_secs(5);
/// 探测一条流最多等多久：源站不回应时不让后面的一直排着。
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// 加载这么久还没找到流时，建议打开网页：有的网页要登录或点了播放才加载视频。
const SUGGEST_PAGE_AFTER: Duration = Duration::from_secs(12);

/// 发给外面的事件。
pub enum SniffEvent {
    /// 下载这条流：打开它的新建页
    Probe(Link),
    Dismissed,
    /// 网页显示或收起了：外面据此决定要不要让出任务列表的位置
    PageToggled,
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

/// 一条流在后台探测到哪一步。
#[derive(Clone)]
enum Check {
    Waiting,
    Running,
    /// 探测成了：说明（几档清晰度、时长或直播）与下载时用的链接
    Ready {
        summary: String,
        link: Link,
    },
    /// 没成：原因；配好了请求头的带着链接（打开新建页可改请求头再试）
    Failed {
        reason: String,
        link: Option<Link>,
    },
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
    check: Check,
    /// 配请求头时没带上的、放不进请求头的 Cookie 个数
    skipped: usize,
}

pub struct SniffView {
    service: Rc<Service>,
    /// 建不出网页时为原因
    webview: Result<Entity<WebView>, String>,
    /// 网页显示着（左边网页、右边列表）；否则只有列表，网页在后台
    page_shown: bool,
    address: Entity<InputState>,
    /// 网页的标题；还没有时为 None
    title: Option<String>,
    found: Vec<Found>,
    /// 列表满了之后又找到的条数
    overflow: usize,
    /// 各个框架（脚本取的编号）发出的请求数
    requests: HashMap<String, u32>,
    /// 网页发来、积压满了或太长而丢掉的消息数，从打开起算
    dropped: Rc<Cell<u64>>,
    /// 最近一次操作没成的原因（地址不是链接、网页没能显示等）
    error: Option<String>,
    /// 网页最近一次要在新窗口里打开的地址
    popup: Option<Url>,
    /// 当前页面开始加载的时刻
    loading_since: Instant,
    /// 有一条流正在后台探测（一次只探测一条）
    checking: bool,
    /// 网页换了地址、还没写进地址栏的（写地址栏要窗口，下一次画时写）
    navigated: Option<String>,
    _loading: Task<()>,
    _incoming: Task<()>,
    _checking: Task<()>,
    /// 加载够久时重画一次，好给出打开网页的建议
    _suggest: Task<()>,
    _typed: Subscription,
}

impl EventEmitter<SniffEvent> for SniffView {}

/// 把网页发来的交给处理它的任务：消息太长、积压满了时丢掉并计数，标题先截短；关了时什么也不做。
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
    /// 在后台打开网页 `url`、开始找视频流；网页不显示。
    pub fn new(
        service: Rc<Service>,
        url: Url,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (loading, mut loadings) = mpsc::unbounded_channel::<Loading>();
        let (incoming, mut incomings) = mpsc::channel::<Incoming>(QUEUE);
        let dropped = Rc::new(Cell::new(0));
        let builder = wry::WebViewBuilder::new()
            .with_visible(false)
            // 后台的网页不自动播放：没有用户点过的媒体不出声；播放器多在初始化时就拉播放列表，照样找得到
            .with_autoplay(false)
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
                // 关了时没有人收
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
        let mut view = SniffView {
            service,
            webview,
            page_shown: false,
            address,
            title: None,
            found: Vec::new(),
            overflow: 0,
            requests: HashMap::new(),
            dropped,
            error: None,
            popup: None,
            loading_since: Instant::now(),
            checking: false,
            navigated: None,
            _loading: loading_task,
            _incoming: incoming_task,
            _checking: Task::ready(()),
            _suggest: Task::ready(()),
            _typed: typed,
        };
        view.wait_to_suggest(cx);
        view
    }

    /// 从现在起加载 [`SUGGEST_PAGE_AFTER`] 后重画一次：那时还没找到流就建议打开网页。
    fn wait_to_suggest(&mut self, cx: &mut Context<Self>) {
        self.loading_since = Instant::now();
        self._suggest = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SUGGEST_PAGE_AFTER).await;
            // 关了时不用重画
            let _closed = this.update(cx, |_, cx| cx.notify());
        });
    }

    fn on_loading(&mut self, event: Loading, cx: &mut Context<Self>) {
        match event {
            Loading::Started(url) => {
                self.found.clear();
                self.overflow = 0;
                self.requests.clear();
                self.title = None;
                self.error = None;
                self.popup = None;
                self.checking = false;
                self._checking = Task::ready(());
                self.navigated = Some(url);
                self.wait_to_suggest(cx);
            }
            Loading::Finished(url) => self.navigated = Some(url),
        }
        cx.notify();
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
                            check: Check::Waiting,
                            skipped: 0,
                        });
                        self.check_next(cx);
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

    /// 后台探测下一条还没探测的流：读出网页引擎里发往它的 Cookie、配好请求头、探测，一次一条。
    fn check_next(&mut self, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        let Some(webview) = self.webview() else {
            return;
        };
        let raw = webview.read(cx).raw();
        let top = match raw.url() {
            Ok(top) => http_url(&top),
            Err(error) => {
                self.error = Some(format!("读不出网页的地址：{error}"));
                return;
            }
        };
        let Some(found) = self
            .found
            .iter_mut()
            .find(|found| matches!(found.check, Check::Waiting))
        else {
            return;
        };
        found.check = Check::Running;
        let found = found.clone();
        let asking = hs_m3u8_webview::cookies::cookies_for(raw, &found.url);
        let (service, title) = (self.service.clone(), self.title.clone());
        self.checking = true;
        self._checking = cx.spawn(async move |this, cx| {
            let asking = std::pin::pin!(asking);
            let cookies = within(asking, cx.background_executor().timer(COOKIE_TIMEOUT))
                .await
                .unwrap_or_else(|| {
                    Err(format!(
                        "网页引擎 {} 秒没有回出 Cookie",
                        COOKIE_TIMEOUT.as_secs()
                    ))
                });
            let made = cookies.and_then(|cookies| {
                request::headers_for(&request::Found {
                    stream: &found.url,
                    frame: found.frame.as_ref(),
                    origin: found.origin.as_ref(),
                    agent: &found.agent,
                    top: top.as_ref(),
                    cookies,
                })
            });
            let (check, skipped) = match made {
                Err(reason) => (Check::Failed { reason, link: None }, 0),
                Ok(made) => {
                    let link = Link {
                        url: found.url.clone(),
                        title,
                        headers: made.headers,
                    };
                    let mut source = Source::new(found.url.clone());
                    source.http.headers.clone_from(&link.headers);
                    // 超时后不再等；探测本身照常在服务里做完
                    let probing = std::pin::pin!(service.probe(source));
                    let timer = cx.background_executor().timer(PROBE_TIMEOUT);
                    let check = match within(probing, timer).await {
                        Some(Ok(Ok(probe))) => Check::Ready {
                            summary: new_task::describe(&probe).join(" · "),
                            link,
                        },
                        Some(Ok(Err(error))) => Check::Failed {
                            reason: Failure::of(&error).message,
                            link: Some(link),
                        },
                        Some(Err(join)) => Check::Failed {
                            reason: format!("探测中途出错：{join}"),
                            link: Some(link),
                        },
                        None => Check::Failed {
                            reason: format!("探测 {} 秒没有结果", PROBE_TIMEOUT.as_secs()),
                            link: Some(link),
                        },
                    };
                    (check, made.skipped)
                }
            };
            // 关了时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                this.checking = false;
                // 网页在探测期间换了页时，这条已不在列表里
                if let Some(entry) = this.found.iter_mut().find(|entry| entry.url == found.url) {
                    entry.check = check;
                    entry.skipped = skipped;
                }
                this.check_next(cx);
                cx.notify();
            });
        });
    }

    /// 网页的标题；还没有时为 None。
    pub fn page_title(&self) -> Option<String> {
        self.title.clone()
    }

    /// 列出了几条流。
    pub fn found_count(&self) -> usize {
        self.found.len()
    }

    /// 网页显示着：外面这时让出任务列表的位置。
    pub fn page_shown(&self) -> bool {
        self.page_shown
    }

    fn webview(&self) -> Option<&Entity<WebView>> {
        self.webview.as_ref().ok()
    }

    /// 显示或收起网页。收起时原生网页要自己藏起来（不随 GPUI 的画面消失）；显示时随下一次画摆出来。
    fn show_page(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.page_shown = shown;
        if !shown
            && let Some(webview) = self.webview()
            && let Err(error) = webview.read(cx).hide()
        {
            self.error = Some(format!("网页没能收起：{error}"));
        }
        cx.emit(SniffEvent::PageToggled);
        cx.notify();
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

    /// 不显示了（换到别的页）：藏起网页、暂停里面的视频与音频；后台探测照常做完。
    pub fn hide(&mut self, cx: &mut Context<Self>) {
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

    /// 又显示了：恢复播放（网页显示着时随下一次画重新摆出来）。
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
        let body = div().flex_1().flex().gap(px(2. * GAP)).pb(px(ui::BOTTOM));
        if self.page_shown {
            body.child(self.render_page(cx)).child(
                self.render_list(cx)
                    .flex_basis(px(ASIDE_WIDTH))
                    .flex_shrink(1.)
                    .min_w(px(ASIDE_MIN_WIDTH)),
            )
        } else {
            body.justify_center().child(
                self.render_list(cx)
                    .flex_1()
                    .max_w(px(LIST_MAX_WIDTH))
                    .pt(px(8.)),
            )
        }
    }
}

impl SniffView {
    /// 网页一栏：工具栏与网页。
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
                "hide-page",
                "收起网页",
                Tone::Ghost,
                cx.listener(|this, _, _, cx| this.show_page(false, cx)),
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

    /// 视频流一栏：网页的标题与主机、找到的流、说明与操作。
    fn render_list(&self, cx: &mut Context<Self>) -> Div {
        let requests: u64 = self.requests.values().map(|&count| u64::from(count)).sum();
        let first_ready = self
            .found
            .iter()
            .position(|found| matches!(found.check, Check::Ready { .. }));
        let rows: Vec<AnyElement> = self
            .found
            .iter()
            .enumerate()
            .map(|(index, found)| self.render_row(index, found, first_ready == Some(index), cx))
            .collect();
        let suggest = self.found.is_empty() && self.loading_since.elapsed() >= SUGGEST_PAGE_AFTER;
        let empty = self.found.is_empty().then(|| {
            div()
                .text_size(px(13.))
                .line_height(px(TEXT_LINE))
                .text_color(theme::muted())
                .child(if suggest {
                    "还没找到。有的网页要登录、或点了播放才加载视频：打开网页弄好后，这里会列出来。"
                } else {
                    "正在网页里找视频流…"
                })
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
                "网页发来的消息太多或太长，丢了 {dropped} 条，可能有流没列出"
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
        let actions = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .when(!self.page_shown, |actions| {
                actions
                    .child(ui::button(
                        "show-page",
                        "打开网页",
                        if suggest { Tone::Glow } else { Tone::Plain },
                        cx.listener(|this, _, _, cx| this.show_page(true, cx)),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .child(ui::caption("要登录、认证或手动点播放时打开")),
                    )
            })
            .when(self.page_shown, |actions| actions.child(div().flex_1()))
            .child(ui::button(
                "close-sniff",
                "关闭",
                Tone::Ghost,
                cx.listener(|_, _, _, cx| cx.emit(SniffEvent::Dismissed)),
            ));
        let host = self
            .webview()
            .and_then(|webview| webview.read(cx).raw().url().ok())
            .and_then(|url| http_url(&url))
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default();
        div()
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
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::BOLD)
                                    .truncate()
                                    .child(
                                        self.title
                                            .clone()
                                            .unwrap_or_else(|| "网页里的视频流".to_owned()),
                                    ),
                            )
                            .child(ui::num(ui::caption(host))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(8.))
                            .child(ui::dot(theme::cyan(), 7.))
                            .child(ui::num(ui::caption(format!(
                                "后台加载 · {requests} 个请求"
                            )))),
                    ),
            )
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
            .child(actions)
    }

    /// 一条流：同心环、名字、地址与探测说明；探测好的可以下载，没成而配好了请求头的可以打开新建页改请求头再试。
    fn render_row(
        &self,
        index: usize,
        found: &Found,
        first: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (mut detail, failed) = match &found.check {
            Check::Waiting => ("等待探测".to_owned(), false),
            Check::Running => ("探测中…".to_owned(), false),
            Check::Ready { summary, .. } => (summary.clone(), false),
            Check::Failed { reason, .. } => (reason.clone(), true),
        };
        if found.skipped > 0 {
            detail.push_str(&format!(
                " · 没带 {} 个 Cookie（没有名字或含控制字符）",
                found.skipped
            ));
        }
        let action = match &found.check {
            Check::Ready { link, .. } => {
                Some(("下载", if first { Tone::Glow } else { Tone::Plain }, link))
            }
            Check::Failed {
                link: Some(link), ..
            } => Some(("查看", Tone::Plain, link)),
            Check::Waiting | Check::Running | Check::Failed { link: None, .. } => None,
        }
        .map(|(label, tone, link)| {
            let link = link.clone();
            ui::button(
                ("download-found", index),
                label,
                tone,
                cx.listener(move |_, _, _, cx| cx.emit(SniffEvent::Probe(link.clone()))),
            )
        });
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
                    ))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(CAPTION_LINE))
                            .text_color(if failed {
                                theme::orange()
                            } else {
                                theme::soft()
                            })
                            .line_clamp(2)
                            .child(detail),
                    ),
            )
            .children(action)
            .into_any_element()
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
