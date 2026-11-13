//! 新建任务：带着请求头探测地址，每档清晰度画成一个同心环（越粗码率越高，引线连到右侧的标签），点环或用方向键
//! 选中，再选音轨、输出、保存位置与请求头。探测被拒时可改请求头重新探测；请求经哪个代理写在选项栏里。

mod rings;
mod side;

use self::rings::{FIGURE_HEIGHT, FIGURE_WIDTH};

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, PathPromptOptions, Pixels, Render, Styled as _,
    Task, Window, div, px,
};
use hs_m3u8_app::{Draft, Failure, FailureKind, OutputKind, Service, task_title};
use hs_m3u8_core::hls::{AudioChoice, Preference, VariantChoice};
use hs_m3u8_core::{Probe, ProxySource, Source, Url};

use crate::headers::HeadersEditor;
use crate::ui::{BOTTOM, GAP};

/// 窗口窄时选项栏最多收到的宽度：放得下一行「MP4 · 本地 HLS · 两者」三个胶囊。
const OPTION_MIN_WIDTH: f32 = 260.;
/// 新建页的最小宽：整个同心环区域与收到最窄的选项栏。
pub const MIN_WIDTH: f32 = FIGURE_WIDTH + GAP + OPTION_MIN_WIDTH;
/// 新建页的最小高：同心环区域与下边距。
pub const MIN_HEIGHT: f32 = FIGURE_HEIGHT + BOTTOM;

/// 要探测的链接。
#[derive(Clone)]
pub struct Link {
    pub url: Url,
    /// 视频所在网页的标题，任务照它取名（见 [`hs_m3u8_app::task_title`]）；没有时由地址得出
    pub title: Option<String>,
    /// 探测与下载时带的请求头
    pub headers: Vec<(String, String)>,
}

/// 新建任务视图发给外面的事件。
pub enum NewTaskEvent {
    /// 按这些选择新建任务；结果经 [`NewTask::failed`] 告诉视图，成功时视图随之关掉
    Create(Box<Draft>),
    Dismissed,
    /// 换了保存位置：之后新建的任务默认也存到这里
    FolderChanged(PathBuf),
    /// 探测完成，可以开始：由外面决定要不要把键盘焦点给它（[`NewTask::focus`]）
    Ready,
}

/// 探测进行到哪一步，粘贴栏与任务列表据此显示状态。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Probing,
    Ready,
    Failed,
}

impl Phase {
    /// 粘贴栏右侧与任务列表里新任务一项的说明。
    pub fn label(self) -> &'static str {
        match self {
            Phase::Probing => "正在探测",
            Phase::Ready => "探测完成",
            Phase::Failed => "探测失败",
        }
    }
}

/// 请求经哪里走。任务都用系统代理设置（核心的默认 [`hs_m3u8_core::Proxy::System`]，应用不改它），这里照同一份设置读。
enum Route {
    /// 经这个代理（已去掉凭据的写法）与读到它的地方
    Proxy(String, &'static str),
    /// 直连，值为原因
    Direct(&'static str),
    /// 系统代理设置用不了或读不出，值为原因；任务开始时同样会失败
    Broken(String),
}

impl Route {
    fn of(url: &Url) -> Route {
        match hs_m3u8_core::system_proxy() {
            Ok(Some(system)) => match system.proxy_for(url) {
                Some(proxy) => Route::Proxy(
                    hs_m3u8_core::bare_url(proxy),
                    match system.source {
                        ProxySource::Environment => "环境变量",
                        ProxySource::MacOs => "系统设置",
                        ProxySource::Windows => "Internet 选项",
                    },
                ),
                None => Route::Direct("这个地址不经代理"),
            },
            Ok(None) => Route::Direct("系统没设代理"),
            Err(error) => Route::Broken(error.to_string()),
        }
    }
}

enum Probing {
    Running,
    Failed(Failure),
    Ready(Box<Probe>),
}

pub struct NewTask {
    service: Rc<Service>,
    url: Url,
    /// 任务的标题，开始时与已有的重名会加编号
    title: String,
    /// 交给服务的网页标题（见 [`Link::title`]）
    page_title: Option<String>,
    probing: Probing,
    /// 选中的变体在 `MasterInfo::variants` 里的下标；来源是媒体播放列表时不用
    variant: usize,
    /// 选中的音频在 `MasterInfo::audio` 里的下标；None 为来源的默认
    audio: Option<usize>,
    /// 鼠标所在的环（变体下标）
    hovered: Option<usize>,
    output: OutputKind,
    folder: PathBuf,
    home: Option<PathBuf>,
    /// 同心环的绘制区域，鼠标事件据此算点在哪个环上
    rings_bounds: Rc<Cell<Bounds<Pixels>>>,
    error: Option<String>,
    /// 点了开始、服务还在新建：开始按钮不可用
    creating: bool,
    /// 额外的请求头
    headers: Entity<HeadersEditor>,
    /// 探测完成后获得焦点：方向键换档、回车开始、Esc 关闭
    focus: FocusHandle,
    /// 这个地址的请求经哪里走：还没读到系统代理时为 None
    route: Option<Route>,
    _route: Task<()>,
    _probe: Task<()>,
}

impl EventEmitter<NewTaskEvent> for NewTask {}

impl NewTask {
    pub fn new(
        service: Rc<Service>,
        link: Link,
        folder: PathBuf,
        home: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Link {
            url,
            title: page_title,
            headers,
        } = link;
        let editor = cx.new(|cx| HeadersEditor::new(headers.clone(), window, cx));
        // 读系统代理要调操作系统的接口，在后台读
        let target = url.clone();
        let reading = cx
            .background_executor()
            .spawn(async move { Route::of(&target) });
        let route = cx.spawn(async move |this, cx| {
            let route = reading.await;
            // 视图在读的期间被关掉时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                this.route = Some(route);
                cx.notify();
            });
        });
        let mut view = NewTask {
            service,
            title: task_title(&url, page_title.as_deref()),
            page_title,
            url,
            probing: Probing::Running,
            variant: 0,
            audio: None,
            hovered: None,
            output: OutputKind::Mp4,
            folder,
            home,
            rings_bounds: Rc::new(Cell::new(Bounds::default())),
            error: None,
            creating: false,
            headers: editor,
            focus: cx.focus_handle(),
            route: None,
            _route: route,
            _probe: Task::ready(()),
        };
        view.probe(headers, cx);
        view
    }

    /// 带着请求头 `headers` 探测（之前的探测结果作废）。
    fn probe(&mut self, headers: Vec<(String, String)>, cx: &mut Context<Self>) {
        let mut source = Source::new(self.url.clone());
        source.http.headers = headers;
        let handle = self.service.probe(source);
        self.probing = Probing::Running;
        self._probe = cx.spawn(async move |this, cx| {
            let probing = match handle.await {
                Ok(Ok(probe)) => Probing::Ready(Box::new(probe)),
                Ok(Err(error)) => Probing::Failed(Failure::of(&error)),
                Err(join) => Probing::Failed(Failure {
                    kind: FailureKind::Other,
                    message: format!("探测中途出错：{join}"),
                }),
            };
            // 视图在探测期间被关掉时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                if let Probing::Ready(probe) = &probing
                    && let Some(master) = &probe.master
                {
                    this.variant = master.selected.variant.index;
                    this.audio = master.selected.audio.as_ref().map(|a| a.index);
                }
                if matches!(probing, Probing::Ready(_)) {
                    cx.emit(NewTaskEvent::Ready);
                }
                this.probing = probing;
                cx.notify();
            });
        });
        cx.notify();
    }

    /// 带上请求头编辑框里的请求头重新探测；输入框里还没按回车的一条加不进去时不探测（原因显示在输入框下面）。
    fn reprobe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(headers) = self
            .headers
            .update(cx, |editor, cx| editor.confirm(window, cx))
        {
            self.probe(headers, cx);
        }
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn phase(&self) -> Phase {
        match self.probing {
            Probing::Running => Phase::Probing,
            Probing::Failed(_) => Phase::Failed,
            Probing::Ready(_) => Phase::Ready,
        }
    }

    /// 让新建页获得键盘焦点：方向键换档、回车开始、Esc 关闭。
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    /// 新建页自己有焦点时的按键：↑ ← 换到码率高一档（外一环），↓ → 换到低一档，回车开始，Esc 关闭。
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // 开始了就不能取消，也不再换档
        if !self.focus.is_focused(window) || self.creating {
            return;
        }
        match event.keystroke.key.as_str() {
            "up" | "left" => self.step_variant(-1, cx),
            "down" | "right" => self.step_variant(1, cx),
            "enter" => self.start(window, cx),
            "escape" => cx.emit(NewTaskEvent::Dismissed),
            _ => {}
        }
    }

    /// 按环由外向内的顺序从选中的一档挪 `step` 档；到头了不动。
    fn step_variant(&mut self, step: isize, cx: &mut Context<Self>) {
        let Probing::Ready(probe) = &self.probing else {
            return;
        };
        let Some(master) = &probe.master else {
            return;
        };
        let order: Vec<usize> = rings::rings(&master.variants)
            .iter()
            .map(|ring| ring.index)
            .collect();
        let Some(at) = order.iter().position(|&index| index == self.variant) else {
            return;
        };
        if let Some(&index) = at.checked_add_signed(step).and_then(|to| order.get(to)) {
            self.select_variant(index, cx);
        }
    }

    fn select_variant(&mut self, index: usize, cx: &mut Context<Self>) {
        self.variant = index;
        // 音频组换了时回到来源的默认音频
        if let Probing::Ready(probe) = &self.probing
            && let Some(master) = &probe.master
        {
            let group = master.variants[index].audio_group.as_deref();
            if let Some(audio) = self.audio
                && Some(master.audio[audio].group.as_str()) != group
            {
                self.audio = None;
            }
        }
        cx.notify();
    }

    fn change_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("存到这里".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = picked.await;
            // 选择期间视图被关掉时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(mut paths))) if !paths.is_empty() => {
                        let folder = paths.swap_remove(0);
                        this.folder = folder.clone();
                        cx.emit(NewTaskEvent::FolderChanged(folder));
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => this.error = Some(format!("打不开目录选择：{error}")),
                    Err(_) => this.error = Some("目录选择没有返回结果".to_owned()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 交出选择，由主窗口新建任务；请求头输入框里还没按回车的一条加不进去时不交，原因显示在输入框下面。
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Probing::Ready(probe) = &self.probing else {
            return;
        };
        if self.creating {
            return;
        }
        let Some(headers) = self
            .headers
            .update(cx, |editor, cx| editor.confirm(window, cx))
        else {
            return;
        };
        let preference = match &probe.master {
            Some(master) => Preference {
                variant: master.variants[self.variant].choice(),
                audio: self
                    .audio
                    .map_or(AudioChoice::Default, |index| master.audio[index].choice()),
            },
            None => Preference {
                variant: VariantChoice::Best,
                audio: AudioChoice::Default,
            },
        };
        let draft = Draft {
            url: self.url.clone(),
            title: self.page_title.clone(),
            preference,
            headers,
            output: self.output,
            folder: self.folder.clone(),
            live: probe.live,
        };
        self.error = None;
        cx.emit(NewTaskEvent::Create(Box::new(draft)));
    }

    /// 服务开始新建了。
    pub fn creating(&mut self, cx: &mut Context<Self>) {
        self.creating = true;
        cx.notify();
    }

    /// 新建没成：写在开始按钮上面，可以改了再开始。
    pub fn failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.creating = false;
        self.error = Some(message);
        cx.notify();
    }
}

impl Render for NewTask {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let figure = self.render_figure(cx);
        let side = self.render_side(cx);
        // 中间一栏至少放得下整个同心环区域：窗口窄时先收两侧的栏
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .flex_1()
            .min_w(px(MIN_WIDTH))
            .flex()
            .gap(px(GAP))
            .child(
                div()
                    .flex_1()
                    .min_w(px(FIGURE_WIDTH))
                    .flex()
                    .items_center()
                    .justify_center()
                    .pb(px(BOTTOM))
                    .child(figure),
            )
            .child(side)
    }
}

/// 第 0 条轨（所选变体或来源本身）的时长；直播为当前窗口的。
fn duration_of(probe: &Probe) -> Duration {
    probe.tracks.first().map_or(Duration::ZERO, |t| t.duration)
}
