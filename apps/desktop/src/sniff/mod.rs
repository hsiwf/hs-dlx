//! 嗅探页：左边是网页（后退、前进、刷新、地址栏），右边列出这个页面里找到的视频流。点某条流的「探测」时带上它所在
//! 页面的地址（Referer）、网页的 User-Agent 与网页里这个流地址的 Cookie，交给新建页。

mod script;

use std::collections::HashMap;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Div, Entity, EventEmitter, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use hs_m3u8_app::title_of;
use hs_m3u8_core::{Url, check_option_header};

use self::script::Report;
use crate::links::http_url;
use crate::theme;
use crate::ui::{self, CAPTION_LINE, GAP, TEXT_LINE, Tone};
use crate::webview::WebView;

/// 右边一栏的宽度。
const ASIDE_WIDTH: f32 = 380.;
/// 窗口窄时右边一栏最多收到的宽度。
pub const ASIDE_MIN_WIDTH: f32 = 300.;
/// 网页一栏最窄多宽：常见网页的移动版布局从这里起。
pub const PAGE_MIN_WIDTH: f32 = 480.;

/// 嗅探页发给外面的事件。
pub enum SniffEvent {
    /// 带着这些请求头探测这个流
    Probe {
        url: Url,
        headers: Vec<(String, String)>,
    },
    Dismissed,
}

/// 找到的一条流。
struct Found {
    url: Url,
    /// 它所在框架的地址
    page: Url,
    /// 网页的 User-Agent
    agent: String,
}

pub struct SniffView {
    /// 建不出网页时为原因
    webview: Result<Entity<WebView>, String>,
    address: Entity<InputState>,
    title: String,
    found: Vec<Found>,
    /// 各层框架（按地址区分）发出的请求数
    requests: HashMap<String, u32>,
    /// 最近一次操作没成的原因（地址不是链接、取不到 Cookie 等）
    error: Option<String>,
    /// 网页换了地址、还没写进地址栏的（写地址栏要窗口，下一次画时写）
    navigated: Option<String>,
    _reports: Task<()>,
    _typed: Subscription,
}

impl EventEmitter<SniffEvent> for SniffView {}

impl SniffView {
    pub fn new(url: Url, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
        let builder = wry::WebViewBuilder::new()
            // 播放器常在嵌入的框架里：每一层框架都注入
            .with_initialization_script_for_main_only(script::SCRIPT, false)
            .with_ipc_handler(move |request| {
                // 嗅探页关掉后网页还会发消息，没有人收
                let _closed = sender.send(request.into_body());
            })
            .with_url(url.as_str());
        let webview = WebView::build(builder, window, cx)
            .map(|webview| cx.new(|_| webview))
            .map_err(|error| format!("建不出网页：{error}"));
        let reports = cx.spawn(async move |this, cx| {
            while let Some(message) = receiver.recv().await {
                // 页面发来的按不可信数据校验，认不出的丢掉
                let Some(report) = script::parse(&message) else {
                    continue;
                };
                if this.update(cx, |this, cx| this.apply(report, cx)).is_err() {
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
            title: String::new(),
            found: Vec::new(),
            requests: HashMap::new(),
            error: None,
            navigated: None,
            _reports: reports,
            _typed: typed,
        }
    }

    fn apply(&mut self, report: Report, cx: &mut Context<Self>) {
        match report {
            Report::Stream { url, page, agent } => {
                if !self.found.iter().any(|found| found.url == url) {
                    self.found.push(Found { url, page, agent });
                }
            }
            Report::Requests { frame, count } => {
                self.requests.insert(frame, count);
            }
            Report::Page { url, title } => {
                self.title = title;
                self.navigated = Some(url.to_string());
            }
        }
        cx.notify();
    }

    fn webview(&self) -> Option<&Entity<WebView>> {
        self.webview.as_ref().ok()
    }

    /// 在网页里打开地址栏里的 `text`。
    fn open(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(url) = http_url(text) else {
            self.error = Some("要 http 或 https 开头的链接".to_owned());
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

    /// 在网页里执行后退、前进或刷新。
    fn navigate(&mut self, script: &str, cx: &mut Context<Self>) {
        if let Some(webview) = self.webview() {
            self.error = webview
                .read(cx)
                .raw()
                .evaluate_script(script)
                .err()
                .map(|error| format!("网页没响应：{error}"));
        }
        cx.notify();
    }

    /// 探测第 `index` 条流要带的请求头：它所在页面（去掉 `#` 之后的部分）作 Referer、网页的 User-Agent，与网页里
    /// 这个流地址的 Cookie；都按核心的规则校验过。
    fn headers_for(
        &self,
        index: usize,
        cx: &Context<Self>,
    ) -> Result<Vec<(String, String)>, String> {
        let found = &self.found[index];
        let mut page = found.page.clone();
        page.set_fragment(None);
        let mut headers = vec![("Referer".to_owned(), page.to_string())];
        if !found.agent.is_empty() {
            headers.push(("User-Agent".to_owned(), found.agent.clone()));
        }
        if let Some(webview) = self.webview() {
            let cookies = webview
                .read(cx)
                .raw()
                .cookies_for_url(found.url.as_str())
                .map_err(|error| format!("读不出网页的 Cookie：{error}"))?;
            if !cookies.is_empty() {
                let pairs: Vec<String> = cookies
                    .iter()
                    .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
                    .collect();
                headers.push(("Cookie".to_owned(), pairs.join("; ")));
            }
        }
        for (name, value) in &headers {
            check_option_header(name, value)?;
        }
        Ok(headers)
    }

    fn probe(&mut self, index: usize, cx: &mut Context<Self>) {
        match self.headers_for(index, cx) {
            Ok(headers) => {
                self.error = None;
                cx.emit(SniffEvent::Probe {
                    url: self.found[index].url.clone(),
                    headers,
                });
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    /// 把原生网页藏起来：嗅探页不显示时（换成新建页）它不会自己藏，再画时重新摆出来。
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if let Some(webview) = self.webview()
            && let Err(error) = webview.read(cx).raw().set_visible(false)
        {
            self.error = Some(format!("网页没能藏起来：{error}"));
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
                   script: &'static str,
                   cx: &mut Context<Self>| {
            ui::button(
                id,
                label,
                Tone::Ghost,
                cx.listener(move |this, _, _, cx| this.navigate(script, cx)),
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
            .child(nav("back", "‹", "history.back();", cx))
            .child(nav("forward", "›", "history.forward();", cx))
            .child(nav("reload", "↻", "location.reload();", cx))
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
        let requests: u32 = self.requests.values().sum();
        let rows = self.found.iter().enumerate().map(|(index, found)| {
            div()
                .flex()
                .items_center()
                .gap(px(14.))
                .p(px(12.))
                .rounded(px(14.))
                .border_1()
                .border_color(theme::edge())
                .bg(theme::panel())
                .child(rings())
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
                .child(ui::button(
                    ("probe-found", index),
                    "探测",
                    if index == 0 { Tone::Glow } else { Tone::Plain },
                    cx.listener(move |this, _, _, cx| this.probe(index, cx)),
                ))
        });
        let empty = self.found.is_empty().then(|| {
            div()
                .text_size(px(13.))
                .line_height(px(TEXT_LINE))
                .text_color(theme::muted())
                .child("还没找到。在网页里点播放，播放器拉取的 m3u8 会出现在这里。")
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
            .children((!self.title.is_empty()).then(|| ui::caption(self.title.clone())))
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
            .children(self.error.clone().map(ui::error_text))
            .child(ui::caption(
                "探测时带上这个页面的地址、User-Agent 与 Cookie；Cookie 只发给流地址所在的网站。",
            ))
    }
}

/// 流前面的小同心环：两环，表示一个可以探测出几档清晰度的地址。
fn rings() -> Div {
    let ring = |size: f32, width: f32, color| {
        div()
            .absolute()
            .left(px((RINGS - size) / 2.))
            .top(px((RINGS - size) / 2.))
            .size(px(size))
            .rounded_full()
            .border(px(width))
            .border_color(color)
    };
    div()
        .relative()
        .size(px(RINGS))
        .flex_none()
        .child(ring(RINGS, 4., theme::cyan()))
        .child(ring(RINGS - 18., 2., theme::cyan_dim()))
}

/// 流的说明：主机与路径；只差查询参数的几条（常见：同一个地址带不同的令牌）标出带参数的。
fn meta(url: &Url) -> String {
    let mut text = format!("{}{}", url.host_str().unwrap_or_default(), url.path());
    if url.query().is_some() {
        text.push_str(" · 带参数");
    }
    text
}

/// 流前面的小同心环的边长。
const RINGS: f32 = 44.;
