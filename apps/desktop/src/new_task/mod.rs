//! 新建任务：探测地址，每档清晰度画成一个同心环（越粗码率越高，引线连到右侧的标签），点环选中，再选音轨、输出与
//! 保存位置。

mod rings;
mod side;

use self::rings::{FIGURE_HEIGHT, FIGURE_WIDTH};

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{
    Bounds, Context, EventEmitter, IntoElement, ParentElement as _, PathPromptOptions, Pixels,
    Render, Styled as _, Task, Window, div, px,
};
use hs_m3u8_app::{Draft, OutputKind, Service};
use hs_m3u8_core::hls::{AudioChoice, Preference, VariantChoice};
use hs_m3u8_core::{Probe, Source, Url};

use crate::ui::{BOTTOM, GAP};

/// 窗口窄时选项栏最多收到的宽度：放得下一行「MP4 · 本地 HLS · 两者」三个胶囊。
const OPTION_MIN_WIDTH: f32 = 260.;
/// 新建页的最小宽：整个同心环区域与收到最窄的选项栏。
pub const MIN_WIDTH: f32 = FIGURE_WIDTH + GAP + OPTION_MIN_WIDTH;
/// 新建页的最小高：同心环区域与下边距。
pub const MIN_HEIGHT: f32 = FIGURE_HEIGHT + BOTTOM;

/// 新建任务视图发给外面的事件。
pub enum NewTaskEvent {
    /// 按这些选择新建任务；结果经 [`NewTask::failed`] 告诉视图，成功时视图随之关掉
    Create(Box<Draft>),
    Dismissed,
    /// 换了保存位置：之后新建的任务默认也存到这里
    FolderChanged(PathBuf),
}

/// 探测进行到哪一步，粘贴栏与任务列表据此显示状态。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Probing,
    Ready,
    Failed,
}

impl Phase {
    /// 粘贴栏右侧的状态。
    pub fn short(self) -> &'static str {
        match self {
            Phase::Probing => "正在探测",
            Phase::Ready => "探测完成",
            Phase::Failed => "探测失败",
        }
    }

    /// 任务列表里新任务一项的说明。
    pub fn label(self) -> &'static str {
        match self {
            Phase::Probing => "正在探测",
            Phase::Ready => "探测完成",
            Phase::Failed => "探测失败",
        }
    }
}

enum Probing {
    Running,
    Failed(String),
    Ready(Box<Probe>),
}

pub struct NewTask {
    url: Url,
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
    _probe: Task<()>,
}

impl EventEmitter<NewTaskEvent> for NewTask {}

impl NewTask {
    pub fn new(
        service: Rc<Service>,
        url: Url,
        folder: PathBuf,
        home: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let handle = service.probe(Source::new(url.clone()));
        let probe = cx.spawn(async move |this, cx| {
            let probing = match handle.await {
                Ok(Ok(probe)) => Probing::Ready(Box::new(probe)),
                Ok(Err(error)) => Probing::Failed(error.to_string()),
                Err(join) => Probing::Failed(format!("探测中途出错：{join}")),
            };
            // 视图在探测期间被关掉时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                if let Probing::Ready(probe) = &probing
                    && let Some(master) = &probe.master
                {
                    this.variant = master.selected.variant.index;
                    this.audio = master.selected.audio.as_ref().map(|a| a.index);
                }
                this.probing = probing;
                cx.notify();
            });
        });
        NewTask {
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
            _probe: probe,
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

    /// 交出选择，由主窗口新建任务。
    fn start(&mut self, cx: &mut Context<Self>) {
        let Probing::Ready(probe) = &self.probing else {
            return;
        };
        if self.creating {
            return;
        }
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
            preference,
            headers: Vec::new(),
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
