//! 网页里的视频流：网页在后台加载、不显示，主体列出找到的视频流，每条在后台探测好（几档清晰度、时长或直播），用户
//! 挑一条下载。要登录、认证或手动点播放时可以打开网页（左边网页、右边列表），弄完收起。
//!
//! 页面的地址、加载与标题由网页引擎报；脚本发回的只有找到的流与文档类型，按不可信数据校验、限量（见
//! [`script`]）。网页要开的新窗口与要下载的文件都不放行，列出来由用户决定。网页里的视频与音频只在网页显示着时放。

mod check;
mod found;
mod list;
mod page;
mod request;
mod script;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, Focusable as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Task, Window, div, px,
};
use hs_m3u8_app::Service;
use hs_m3u8_core::Url;
use hs_m3u8_webview::WebView;
use tokio::sync::mpsc;

use self::found::{Check, Found};
use self::script::Report;
use crate::links::{NOT_HTTP, http_url};
use crate::new_task::Link;
use crate::ui::{self, GAP};

/// 一个页面里最多列出多少条流：网页能报任意多条，再多的只计数。
const MAX_FOUND: usize = 200;
/// 网页发来、还没处理的消息最多积多少条：网页发得比处理得快时，再来的丢掉并计数。页面加载的事件不计、不丢。
const QUEUE: usize = 256;
/// 加载这么久还没找到流时给出建议：没开始显示的多半打不开，显示了的多半要登录或点了播放才加载视频。
const SUGGEST_AFTER: Duration = Duration::from_secs(12);
/// 显示网页时可以让出的文档类型：网页引擎当作网页显示的。
const PAGE_TYPES: [&str; 2] = ["text/html", "application/xhtml+xml"];

/// 发给外面的事件。
pub enum SniffEvent {
    /// 打开这条流的新建页（带着后台已探测好的结果，见 [`Link::probe`]）
    Open(Link),
    Dismissed,
    /// 网页显示或收起了：外面据此决定要不要让出任务列表的位置
    PageToggled,
    /// 打开的地址是不是网页有了结论（[`SniffView::verdict`]）
    Decided,
}

/// 打开的地址是不是网页：按网页引擎打开它后建出的第一个顶层文档判断，之后的换页不改。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// 还不知道：顶层文档还没建出
    Pending,
    /// 网页（HTML）
    Page,
    /// 不是网页：网页引擎认出的类型（如 `video/mp4`）；文档加载完也没报类型时为 None
    NotPage(Option<String>),
    /// 建不出网页，或网页进程在有结论之前退出：原因
    Broken(String),
}

/// 网页引擎报来的事。页面加载按发生的先后处理、不丢；别的网页能任意多发，积多了丢（见 [`Inbox`]）。
enum Engine {
    /// 开始显示这个地址（导航已提交）：之前找到的都属于上一个页面
    Started(String),
    Finished(String),
    /// 网页进程退出了（macOS）
    Crashed,
    /// 脚本发回的消息与发它的框架的地址（网页引擎给的，见 [`hs_m3u8_webview::ipc_frame`]）
    Message {
        body: String,
        frame: Option<String>,
    },
    Title(String),
    /// 网页要在新窗口里打开的地址（链接的 target=_blank、window.open）
    Popup(String),
    /// 网页要下载的文件的地址
    Download(String),
}

impl Engine {
    /// 积多了能丢的。
    fn droppable(&self) -> bool {
        !matches!(
            self,
            Engine::Started(_) | Engine::Finished(_) | Engine::Crashed
        )
    }
}

/// 网页引擎报来、还没处理的事：一条按先后的通道，能丢的另计数、限量。
struct Inbox {
    sender: mpsc::UnboundedSender<Engine>,
    /// 在通道里的能丢的事的条数
    pending: Cell<usize>,
    /// 积压满了或太长而丢掉的条数，从打开起算
    dropped: Cell<u64>,
}

impl Inbox {
    /// 交给处理它的任务：能丢的积满了、消息太长时丢掉并计数，标题先截短；视图关了时什么也不做。
    fn offer(&self, event: Engine) {
        let event = match event {
            Engine::Message { body, .. } if body.len() > script::MAX_MESSAGE => {
                self.dropped.set(self.dropped.get() + 1);
                return;
            }
            Engine::Title(title) => Engine::Title(script::clip(&title)),
            event => event,
        };
        if event.droppable() {
            if self.pending.get() >= QUEUE {
                self.dropped.set(self.dropped.get() + 1);
                return;
            }
            self.pending.set(self.pending.get() + 1);
        }
        // 视图关了时没有人收
        let _closed = self.sender.send(event);
    }
}

/// 当前页面加载到哪一步。
struct Load {
    /// 开始显示的顶层地址，网页引擎报的；还没开始显示时为 None
    shown: Option<String>,
    finished: bool,
    /// 这次打开或换页的时刻
    since: Instant,
}

pub struct SniffView {
    service: Rc<Service>,
    /// 建不出网页时为原因
    webview: Result<Entity<WebView>, String>,
    /// 网页显示着（左边网页、右边列表）；否则只有列表，网页在后台
    page_shown: bool,
    /// 嗅探页正显示着（由外面告诉，见 [`SniffView::set_on_screen`]）
    on_screen: bool,
    address: Entity<InputState>,
    /// 网页的标题；还没有时为 None
    title: Option<String>,
    load: Load,
    /// 网页进程退出了，要刷新才回来
    crashed: bool,
    verdict: Verdict,
    found: Vec<Found>,
    /// 列表满了之后又找到的条数
    overflow: usize,
    inbox: Rc<Inbox>,
    /// 最近一次操作没成的原因（地址不是链接、网页没能显示等）
    error: Option<String>,
    /// 网页最近一次要在新窗口里打开的地址
    popup: Option<Url>,
    /// 网页最近一次要下载的文件的地址
    download: Option<Url>,
    /// 网页换了地址、还没写进地址栏的（写地址栏要窗口，下一次画时写）
    navigated: Option<String>,
    /// 正在后台探测的一条（一次一条）；释放即取消
    checking: Option<Task<()>>,
    _engine: Task<()>,
    /// 加载够久时重画一次，好给出建议
    _suggest: Task<()>,
    _typed: Subscription,
}

impl EventEmitter<SniffEvent> for SniffView {}

impl SniffView {
    /// 在后台打开 `url`、开始找视频流；网页不显示，视频与音频不放。
    pub fn new(
        service: Rc<Service>,
        url: Url,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (sender, mut events) = mpsc::unbounded_channel::<Engine>();
        let inbox = Rc::new(Inbox {
            sender,
            pending: Cell::new(0),
            dropped: Cell::new(0),
        });
        let offer = |make: fn(String) -> Engine| {
            let inbox = inbox.clone();
            move |text: String| inbox.offer(make(text))
        };
        let builder = wry::WebViewBuilder::new()
            .with_visible(false)
            // 不放开自动播放（各引擎照默认）；视频与音频另由网页引擎暂停或静音，见 apply_media
            .with_autoplay(false)
            // 播放器常在嵌入的框架里：每一层框架都注入，脚本自己决定在哪层跑（见 script::SCRIPT）
            .with_initialization_script_for_main_only(script::SCRIPT, false)
            .with_ipc_handler({
                let inbox = inbox.clone();
                move |request| {
                    let frame = hs_m3u8_webview::ipc_frame(&request);
                    let body = request.into_body();
                    inbox.offer(Engine::Message { body, frame });
                }
            })
            .with_document_title_changed_handler(offer(Engine::Title))
            .with_new_window_req_handler({
                let popup = offer(Engine::Popup);
                move |url, _| {
                    popup(url);
                    wry::NewWindowResponse::Deny
                }
            })
            .with_download_started_handler({
                let download = offer(Engine::Download);
                move |url, _| {
                    download(url);
                    false
                }
            })
            .with_on_page_load_handler({
                let inbox = inbox.clone();
                move |event, url| {
                    inbox.offer(match event {
                        wry::PageLoadEvent::Started => Engine::Started(url),
                        wry::PageLoadEvent::Finished => Engine::Finished(url),
                    });
                }
            });
        #[cfg(target_os = "macos")]
        let builder = {
            use wry::WebViewBuilderExtDarwin as _;
            let inbox = inbox.clone();
            builder.with_on_web_content_process_terminate_handler(move || {
                inbox.offer(Engine::Crashed);
            })
        };
        let builder = builder.with_url(url.as_str());
        let webview = WebView::build(builder, window, cx)
            .map(|webview| cx.new(|_| webview))
            .map_err(|error| format!("建不出网页：{error}"));
        let engine = cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| this.on_engine(event, cx))
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
        let verdict = match &webview {
            Ok(_) => Verdict::Pending,
            Err(reason) => Verdict::Broken(reason.clone()),
        };
        let mut view = SniffView {
            service,
            webview,
            page_shown: false,
            on_screen: false,
            address,
            title: None,
            load: Load {
                shown: None,
                finished: false,
                since: Instant::now(),
            },
            crashed: false,
            verdict,
            found: Vec::new(),
            overflow: 0,
            inbox,
            error: None,
            popup: None,
            download: None,
            navigated: None,
            checking: None,
            _engine: engine,
            _suggest: Task::ready(()),
            _typed: typed,
        };
        view.apply_media(cx);
        view.wait_to_suggest(cx);
        view
    }

    /// 从现在起 [`SUGGEST_AFTER`] 后重画一次：那时还没找到流就给出建议；打开的地址到那时还没开始显示的，按打不开定论。
    fn wait_to_suggest(&mut self, cx: &mut Context<Self>) {
        self.load.since = Instant::now();
        self._suggest = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SUGGEST_AFTER).await;
            // 关了时不用重画
            let _closed = this.update(cx, |this, cx| {
                if this.verdict == Verdict::Pending && this.load.shown.is_none() {
                    this.decide(
                        Verdict::Broken(format!("网页 {} 秒还没打开", SUGGEST_AFTER.as_secs())),
                        cx,
                    );
                }
                cx.notify();
            });
        });
    }

    fn on_engine(&mut self, event: Engine, cx: &mut Context<Self>) {
        if event.droppable() {
            let pending = self.inbox.pending.get();
            self.inbox
                .pending
                .set(pending.checked_sub(1).expect("能丢的事入队时计过数"));
        }
        match event {
            Engine::Started(url) => {
                self.found.clear();
                self.overflow = 0;
                self.title = None;
                self.error = None;
                self.popup = None;
                self.download = None;
                self.crashed = false;
                self.checking = None;
                self.load.shown = Some(url.clone());
                self.load.finished = false;
                self.navigated = Some(url);
                self.wait_to_suggest(cx);
                // 新页面照当前是否显示决定放不放
                self.apply_media(cx);
            }
            Engine::Finished(url) => {
                self.load.finished = true;
                self.navigated = Some(url);
                // 文档加载完也没报类型：脚本没在它里面跑（如 PDF），不当网页
                if self.verdict == Verdict::Pending && self.load.shown.is_some() {
                    self.decide(Verdict::NotPage(None), cx);
                }
            }
            Engine::Crashed => {
                self.crashed = true;
                self.checking = None;
                if self.verdict == Verdict::Pending {
                    self.decide(Verdict::Broken("网页进程意外退出了".to_owned()), cx);
                }
            }
            Engine::Message { body, frame } => match script::parse(&body) {
                Some(Report::Stream { url, agent }) => {
                    let top = self.load.shown.as_deref().and_then(http_url);
                    let frame = frame.as_deref().and_then(http_url);
                    self.add(Found::new(url, frame, top, agent), cx);
                }
                Some(Report::Document { content_type }) => {
                    if self.verdict == Verdict::Pending {
                        let page = PAGE_TYPES
                            .iter()
                            .any(|page| content_type.eq_ignore_ascii_case(page));
                        self.decide(
                            if page {
                                Verdict::Page
                            } else {
                                Verdict::NotPage(Some(content_type))
                            },
                            cx,
                        );
                    }
                }
                None => return,
            },
            // 单页应用换页时地址变了而不重新加载，常伴着改标题：顺带读一次地址
            Engine::Title(title) => {
                self.title = (!title.is_empty()).then_some(title);
                if let Some(webview) = self.webview() {
                    match webview.read(cx).raw().url() {
                        Ok(url) => self.navigated = Some(url),
                        Err(error) => self.error = Some(format!("读不出网页的地址：{error}")),
                    }
                }
            }
            // 不是 http(s) 的（about:blank、blob: 之类）没什么可开
            Engine::Popup(url) => self.popup = http_url(&url),
            Engine::Download(url) => {
                // 打开的地址本身就是要下载的文件
                if self.verdict == Verdict::Pending && self.load.shown.is_none() {
                    self.decide(Verdict::NotPage(None), cx);
                }
                self.download = http_url(&url);
            }
        }
        cx.notify();
    }

    fn decide(&mut self, verdict: Verdict, cx: &mut Context<Self>) {
        self.verdict = verdict;
        cx.emit(SniffEvent::Decided);
    }

    /// 加进列表并排队探测；同一条流又报来的换成新地址（见 [`Found::renew`]），满了的只计数。
    fn add(&mut self, found: Found, cx: &mut Context<Self>) {
        if let Some(known) = self.found.iter_mut().find(|known| known.key == found.key) {
            known.renew(found);
        } else if self.found.len() < MAX_FOUND {
            self.found.push(found);
        } else {
            self.overflow += 1;
        }
        self.check_next(cx);
    }

    /// 打开的地址是不是网页。
    pub fn verdict(&self) -> &Verdict {
        &self.verdict
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

    /// 嗅探页显示（`on_screen`）或不显示（换到别的页）了。不显示时原生网页要自己藏起来（不随 GPUI 的画面消失）；显示
    /// 时网页随下一次画摆出来。后台探测照常做。
    pub fn set_on_screen(&mut self, on_screen: bool, cx: &mut Context<Self>) {
        self.on_screen = on_screen;
        if !on_screen {
            self.hide_webview(cx);
        }
        self.apply_media(cx);
        cx.notify();
    }

    /// 显示或收起网页。收起即弄完了（登录、点播放）：没成的流带着新的 Cookie 重新探测。
    fn show_page(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.page_shown = shown;
        if !shown {
            self.hide_webview(cx);
            self.retry(None, cx);
        }
        self.apply_media(cx);
        cx.emit(SniffEvent::PageToggled);
        cx.notify();
    }

    fn hide_webview(&mut self, cx: &mut Context<Self>) {
        if let Some(webview) = self.webview()
            && let Err(error) = webview.read(cx).hide()
        {
            self.error = Some(format!("网页没能收起：{error}"));
        }
    }

    /// 网页看得见（嗅探页显示着、网页没收起）时放视频与音频，否则由网页引擎暂停（macOS）或静音（Windows、Linux）。
    fn apply_media(&mut self, cx: &mut Context<Self>) {
        let silenced = !(self.on_screen && self.page_shown);
        if let Some(webview) = self.webview()
            && let Err(error) = webview.read(cx).silence_media(silenced)
        {
            self.error = Some(format!("网页没能停下或恢复播放：{error}"));
        }
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

    /// 打开 `url` 的新建页：探测好的带上结果与当时的请求头，网页的标题取现在的。
    fn open_new_task(&self, url: Url, cx: &mut Context<Self>) {
        let found = self.found.iter().find(|found| found.url == url);
        let (headers, probe) = match found.map(|found| &found.check) {
            Some(Check::Ready { probe, headers, .. }) => (headers.clone(), Some(probe.clone())),
            Some(Check::Failed {
                headers: Some(headers),
                ..
            }) => (headers.clone(), None),
            _ => (Vec::new(), None),
        };
        cx.emit(SniffEvent::Open(Link {
            url,
            title: self.title.clone(),
            headers,
            probe,
        }));
    }
}

impl Render for SniffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 网页换了地址：写进地址栏；正在输入时先留着，输完再写
        if !self.address.read(cx).focus_handle(cx).is_focused(window)
            && let Some(text) = self.navigated.take()
        {
            self.address
                .update(cx, |input, cx| input.set_value(text, window, cx));
        }
        let body = div().flex_1().flex().gap(px(2. * GAP)).pb(px(ui::BOTTOM));
        if self.page_shown {
            body.child(self.render_page(cx))
                .child(page::aside(self.render_list(cx)))
        } else {
            body.justify_center()
                .child(list::alone(self.render_list(cx)))
        }
    }
}
