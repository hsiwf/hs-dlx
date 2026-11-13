//! 新建任务：探测地址，每档清晰度画成一个同心环（越粗码率越高，引线连到右侧的标签），点环选中，再选音轨、输出与
//! 保存位置。

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AbsoluteLength, Bounds, Context, Div, EventEmitter, FontWeight, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, PathPromptOptions, Pixels, Point, Render,
    SharedString, StatefulInteractiveElement as _, Styled, Task, Window, canvas, div, px,
};
use hs_m3u8_app::{Draft, OutputKind, Service, title_of};
use hs_m3u8_core::hls::{AudioChoice, Preference, VariantChoice};
use hs_m3u8_core::{Probe, Source, Url, VariantInfo};

use crate::ui::{
    self, BOTTOM, FIGURE_HEIGHT, FIGURE_WIDTH, GAP, OPTION_WIDTH, SIDE_MIN_WIDTH, Tone,
};
use crate::{format, theme};

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
            Phase::Ready => "探测完成 · 挑一档开始",
            Phase::Failed => "探测失败",
        }
    }
}

enum Probing {
    Running,
    Failed(String),
    Ready(Box<Probe>),
}

/// 输出选项上写的名字。
fn output_label(kind: OutputKind) -> &'static str {
    match kind {
        OutputKind::Mp4 => "MP4",
        OutputKind::Hls => "本地 HLS",
        OutputKind::Both => "两者",
    }
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

/// 同心环的区域边长与它在整块区域里的上边距、最外与最内一环的半径。
const BOX: f32 = 480.;
const BOX_TOP: f32 = 20.;
const OUTER: f32 = 222.;
const INNER: f32 = 102.;
/// 只有一档（来源本身是媒体播放列表）时那一环的半径与宽，探测中占位的暗环的半径。
const SINGLE_RADIUS: f32 = 174.;
const SINGLE_WIDTH: f32 = 14.;
const PLACEHOLDER_RADII: [f32; 3] = [OUTER, 174., 134.];
/// 圆心处写字的方框边长。
const CENTER_BOX: f32 = 180.;
/// 标签所在的横坐标（相对同心环区域）：引线从环上 45° 处水平拉到这里。
const LABEL_X: f32 = 496.;
/// 点在环宽之外多远以内仍算点中这一环。
const HIT_SLOP: f32 = 8.;

/// 一档清晰度的环：变体下标、半径（环宽的中线）与环宽。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ring {
    index: usize,
    radius: f32,
    width: f32,
}

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

    /// 窗口坐标 `position` 落在哪一环上（见 [`hit`]）。
    fn ring_at(&self, position: Point<Pixels>) -> Option<usize> {
        let Probing::Ready(probe) = &self.probing else {
            return None;
        };
        let master = probe.master.as_ref()?;
        let center = self.rings_bounds.get().center();
        let (dx, dy) = (
            f32::from(position.x - center.x),
            f32::from(position.y - center.y),
        );
        hit(&rings(&master.variants), (dx * dx + dy * dy).sqrt())
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

/// 由外向内的各环：码率从高到低，最外一环半径 [`OUTER`]、最内不小于 [`INNER`]、相邻两环的半径差至多 48。环宽按码率
/// 在 4 到 26 之间，且与相邻环留出 8 的缝；档数多到半径差不足 11 时环宽压到 3，缝随之变窄。
fn rings(variants: &[VariantInfo]) -> Vec<Ring> {
    let rates: Vec<(usize, Option<u64>)> =
        variants.iter().map(|v| (v.index, v.bandwidth)).collect();
    rings_of(&rates)
}

/// [`rings`] 的计算：`rates` 为各档的（变体下标, 码率）。
fn rings_of(rates: &[(usize, Option<u64>)]) -> Vec<Ring> {
    let mut order = rates.to_vec();
    order.sort_by_key(|&(_, bandwidth)| std::cmp::Reverse(bandwidth.unwrap_or(0)));
    let top = order.first().and_then(|&(_, bw)| bw).unwrap_or(1).max(1) as f32;
    let step = match order.len() {
        0 | 1 => 0.0,
        n => (48.0_f32).min((OUTER - INNER) / (n - 1) as f32),
    };
    order
        .iter()
        .enumerate()
        .map(|(i, &(index, bandwidth))| {
            let width = 4.0 + 22.0 * bandwidth.unwrap_or(0) as f32 / top;
            let room = if step > 0.0 { step - 8.0 } else { 26.0 };
            Ring {
                index,
                radius: OUTER - step * i as f32,
                width: width.min(room).max(3.0),
            }
        })
        .collect()
}

/// 离圆心 `distance` 的点落在哪一环上：在环宽之外 [`HIT_SLOP`] 以内的环里离得最近的那个；都不在时为 None。
fn hit(rings: &[Ring], distance: f32) -> Option<usize> {
    rings
        .iter()
        .map(|ring| {
            (
                ring.index,
                (distance - ring.radius).abs() - ring.width / 2.0,
            )
        })
        .filter(|(_, gap)| *gap <= HIT_SLOP)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// 设任意宽度的四边边框（GPUI 只给了 0 到 8 的整数档）。
fn border_width<E: Styled>(mut element: E, width: f32) -> E {
    let width: AbsoluteLength = px(width).into();
    let widths = &mut element.style().border_widths;
    widths.top = Some(width);
    widths.right = Some(width);
    widths.bottom = Some(width);
    widths.left = Some(width);
    element
}

/// 一个环：半径 `radius`（环宽中线）、宽 `width`，画在 [`BOX`] 见方区域的中心。
fn ring_div(radius: f32, width: f32) -> Div {
    let size = 2. * radius + width;
    let offset = BOX / 2. - size / 2.;
    border_width(
        div()
            .absolute()
            .left(px(offset))
            .top(px(offset))
            .size(px(size))
            .rounded_full(),
        width,
    )
}

/// 选中的环：青色的环，外加两圈更宽更淡的同心环作光晕。阴影会透过透明的环心铺满整个圆，所以光晕用环画。
fn lit_ring(radius: f32, width: f32) -> Vec<Div> {
    vec![
        ring_div(radius, width + 18.).border_color(theme::alpha(theme::cyan(), 0.06)),
        ring_div(radius, width + 8.).border_color(theme::alpha(theme::cyan(), 0.14)),
        ring_div(radius, width).border_color(theme::cyan()),
    ]
}

impl Render for NewTask {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let figure = self.render_figure(cx);
        let side = self.render_side(cx);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .gap(px(GAP))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .pb(px(BOTTOM))
                    .child(figure),
            )
            .child(side)
    }
}

impl NewTask {
    fn render_figure(&self, cx: &mut Context<Self>) -> Div {
        let (rings, labels, center, center_note) = match &self.probing {
            Probing::Running => (
                // 探测中：三个暗环占位
                PLACEHOLDER_RADII
                    .into_iter()
                    .map(|r| ring_div(r, 4.).border_color(theme::ring_idle()))
                    .collect::<Vec<_>>(),
                Vec::new(),
                "探测中".to_owned(),
                self.url.host_str().unwrap_or_default().to_owned(),
            ),
            Probing::Failed(_) => (
                vec![ring_div(SINGLE_RADIUS, 4.).border_color(theme::orange())],
                Vec::new(),
                "探测失败".to_owned(),
                String::new(),
            ),
            Probing::Ready(probe) => self.ready_rings(probe),
        };
        let bounds_cell = self.rings_bounds.clone();
        let center_color = match self.probing {
            Probing::Failed(_) => theme::orange(),
            _ => theme::text(),
        };
        div()
            .relative()
            .w(px(FIGURE_WIDTH))
            .h(px(FIGURE_HEIGHT))
            .flex_none()
            .child(
                div()
                    .id("rings")
                    .absolute()
                    .left_0()
                    .top(px(BOX_TOP))
                    .size(px(BOX))
                    .when(self.hovered.is_some(), |s| s.cursor_pointer())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui_kit::MouseDownEvent, _, cx| {
                            if let Some(index) = this.ring_at(event.position) {
                                this.select_variant(index, cx);
                            }
                        }),
                    )
                    .on_mouse_move(
                        cx.listener(|this, event: &gpui_kit::MouseMoveEvent, _, cx| {
                            let hovered = this.ring_at(event.position);
                            if hovered != this.hovered {
                                this.hovered = hovered;
                                cx.notify();
                            }
                        }),
                    )
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !*hovered && this.hovered.is_some() {
                            this.hovered = None;
                            cx.notify();
                        }
                    }))
                    .child(
                        canvas(move |bounds, _, _| bounds_cell.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .children(rings)
                    .child(
                        div()
                            .absolute()
                            .left(px(BOX / 2. - CENTER_BOX / 2.))
                            .top(px(BOX / 2. - CENTER_BOX / 2.))
                            .size(px(CENTER_BOX))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(6.))
                            .child(ui::num(
                                div()
                                    .text_size(px(30.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(center_color)
                                    .child(center),
                            ))
                            .child(ui::num(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme::muted())
                                    .child(center_note),
                            )),
                    )
                    .children(labels),
            )
    }

    /// 探测完成时的环、引线与标签，与圆心的大字与小字。
    fn ready_rings(&self, probe: &Probe) -> (Vec<Div>, Vec<Div>, String, String) {
        let duration = duration_of(probe);
        let Some(master) = &probe.master else {
            // 来源本身是媒体播放列表：只有一档，没有可选的。直播的时长与片数是当前窗口的，不写
            let segments = probe.tracks.first().map_or(0, |t| t.segments);
            let (center, note) = if probe.live {
                ("直播".to_owned(), "开始后一直录到停止".to_owned())
            } else {
                (format::clock(duration), format!("{segments} 个分片"))
            };
            return (
                lit_ring(SINGLE_RADIUS, SINGLE_WIDTH),
                Vec::new(),
                center,
                note,
            );
        };
        let rings = rings(&master.variants);
        // 相邻标签的竖向间距不够 16 时只给选中与悬停的环写标签
        let crowded = rings
            .windows(2)
            .any(|pair| (pair[0].radius - pair[1].radius) * std::f32::consts::FRAC_1_SQRT_2 < 16.);
        let c = BOX / 2.;
        let mut ring_divs = Vec::new();
        let mut labels = Vec::new();
        for ring in &rings {
            let on = ring.index == self.variant;
            let hover = self.hovered == Some(ring.index);
            let color = if on {
                theme::cyan()
            } else if hover {
                theme::hover_edge()
            } else {
                theme::ring_idle()
            };
            if on {
                ring_divs.extend(lit_ring(ring.radius, ring.width));
            } else {
                ring_divs.push(ring_div(ring.radius, ring.width).border_color(color));
            }
            if crowded && !on && !hover {
                continue;
            }
            let v = &master.variants[ring.index];
            let anchor = ring.radius * std::f32::consts::FRAC_1_SQRT_2;
            let (x, y) = (c + anchor, c - anchor);
            let text = match v.bandwidth {
                Some(bw) if v.resolution.is_some() => {
                    format!("{} · {}", format::variant(v), format::bitrate(bw))
                }
                _ => format::variant(v),
            };
            labels.push(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(LABEL_X - 4. - x))
                    .h(px(1.))
                    .bg(if on { theme::cyan() } else { theme::edge() }),
            );
            labels.push(
                div()
                    .absolute()
                    .left(px(LABEL_X))
                    .top(px(y - 8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .whitespace_nowrap()
                    .text_size(px(12.))
                    .child(if on {
                        ui::dot(theme::cyan(), 6.)
                    } else {
                        div().size(px(6.)).rounded_full().bg(theme::hover_edge())
                    })
                    .child(ui::num(
                        div()
                            .text_color(if on { theme::text() } else { theme::muted() })
                            .when(on, |s| s.font_weight(FontWeight::SEMIBOLD))
                            .child(text),
                    )),
            );
        }
        let picked = &master.variants[self.variant];
        let size = picked
            .bandwidth
            .filter(|_| !probe.live)
            .map(|bw| format::size_bound(bw, duration))
            .unwrap_or_default();
        (ring_divs, labels, format::variant(picked), size)
    }

    fn render_side(&self, cx: &mut Context<Self>) -> Div {
        let host = self.url.host_str().unwrap_or_default().to_owned();
        let side = div()
            .flex_basis(px(OPTION_WIDTH))
            .flex_shrink(1.)
            .min_w(px(SIDE_MIN_WIDTH))
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(22.))
            .py(px(8.))
            .pr(px(8.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    // 由地址得出的标题；开始时与已有的重名会加编号
                    .child(
                        div()
                            .text_size(px(20.))
                            .line_height(px(28.))
                            .font_weight(FontWeight::BOLD)
                            .line_clamp(2)
                            .child(title_of(&self.url)),
                    )
                    .child(ui::num(ui::caption(self.summary(&host)))),
            );
        match &self.probing {
            Probing::Running => side.child(div().flex_1()).child(ui::button(
                "cancel",
                "取消",
                Tone::Ghost,
                cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed)),
            )),
            Probing::Failed(message) => side
                .child(ui::card(theme::error_edge()).child(SharedString::from(message.clone())))
                .child(div().flex_1())
                .child(ui::button(
                    "dismiss",
                    "返回",
                    Tone::Plain,
                    cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed)),
                )),
            Probing::Ready(probe) => self.ready_side(side, probe, cx),
        }
    }

    /// 标题下的一行：主机 · 时长 · 几档清晰度 · 几条音轨 · 直播。直播的时长是当前窗口的，不写。
    fn summary(&self, host: &str) -> String {
        let Probing::Ready(probe) = &self.probing else {
            return match self.probing {
                Probing::Running => format!("{host} · 正在读取播放列表"),
                _ => host.to_owned(),
            };
        };
        let mut parts = vec![host.to_owned()];
        if !probe.live {
            parts.push(format::clock(duration_of(probe)));
        }
        match &probe.master {
            Some(m) => {
                parts.push(format!("{} 档清晰度", m.variants.len()));
                let audio = m.audio.iter().filter(|a| a.separate).count();
                if audio > 0 {
                    parts.push(format!("{audio} 条音轨"));
                }
            }
            None => parts.push("单一清晰度".to_owned()),
        }
        if probe.live {
            parts.push("直播".to_owned());
        }
        parts.join(" · ")
    }

    /// 探测完成时的右栏：选项多了可以滚动，开始按钮总在底部。
    fn ready_side(&self, side: Div, probe: &Probe, cx: &mut Context<Self>) -> Div {
        let master = probe.master.as_ref();
        let duration = duration_of(probe);
        // （下标、名字、胶囊上写的、选中）
        let audio_options: Vec<(usize, String, String, bool)> = master
            .map(|m| {
                let group = m.variants[self.variant].audio_group.clone();
                m.audio
                    .iter()
                    .filter(|a| a.separate && Some(&a.group) == group.as_ref())
                    .map(|a| {
                        let name = a
                            .name
                            .clone()
                            .or_else(|| a.language.clone())
                            .unwrap_or_else(|| format!("音轨 {}", a.index + 1));
                        let on = self.audio == Some(a.index) || (self.audio.is_none() && a.default);
                        let label = if a.default {
                            format!("{name} · 默认")
                        } else {
                            name.clone()
                        };
                        (a.index, name, label, on)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let audio_label = audio_options
            .iter()
            .find(|(_, _, _, on)| *on)
            .map(|(_, name, _, _)| name.clone());

        let mut options = div()
            .id("options")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(22.));
        if !audio_options.is_empty() {
            let chips = audio_options.into_iter().map(|(index, _, label, on)| {
                ui::chip(
                    ("audio", index),
                    label,
                    on,
                    cx.listener(move |this, _, _, cx| {
                        this.audio = Some(index);
                        cx.notify();
                    }),
                )
            });
            options = options.child(group("音轨", chips));
        }
        let outputs = [OutputKind::Mp4, OutputKind::Hls, OutputKind::Both].map(|kind| {
            ui::chip(
                output_label(kind),
                output_label(kind),
                self.output == kind,
                cx.listener(move |this, _, _, cx| {
                    this.output = kind;
                    cx.notify();
                }),
            )
        });
        options = options.child(group("输出", outputs)).child(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(ui::caption("保存到"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(ui::num(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(format::path(&self.folder, self.home.as_deref())),
                        ))
                        .child(ui::button(
                            "folder",
                            "更改",
                            Tone::Plain,
                            cx.listener(|this, _, _, cx| this.change_folder(cx)),
                        )),
                ),
        );

        let picked = master.map(|m| &m.variants[self.variant]);
        let headline = if self.creating {
            "正在开始…".to_owned()
        } else {
            picked.map_or_else(
                || "开始下载".to_owned(),
                |v| format!("开始下载 · {}", format::variant(v)),
            )
        };
        let detail: Vec<String> = [
            picked
                .and_then(|v| v.bandwidth)
                .filter(|_| !probe.live)
                .map(|bw| format::size_bound(bw, duration)),
            audio_label,
            Some(output_label(self.output).to_owned()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let creating = self.creating;
        side.child(options)
            .children(self.error.clone().map(ui::error_text))
            .child(
                div()
                    .id("start")
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .w_full()
                    .px(px(18.))
                    .py(px(14.))
                    .rounded(px(18.))
                    .bg(theme::cyan())
                    .text_color(theme::on_cyan())
                    .shadow(ui::glow(theme::cyan(), 30., 0.35))
                    .when(creating, |s| s.opacity(0.6))
                    .when(!creating, |s| {
                        s.cursor_pointer()
                            .hover(|s| s.bg(theme::cyan_pale()))
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx)))
                    })
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::BOLD)
                            .child(headline),
                    )
                    .child(ui::num(div().text_size(px(12.)).child(detail.join(" · ")))),
            )
            .child(div().flex().justify_center().child(ui::button(
                "cancel",
                "取消",
                Tone::Ghost,
                cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed)),
            )))
    }
}

/// 第 0 条轨（所选变体或来源本身）的时长；直播为当前窗口的。
fn duration_of(probe: &Probe) -> Duration {
    probe.tracks.first().map_or(Duration::ZERO, |t| t.duration)
}

/// 带小标题的一组胶囊。
fn group(title: &'static str, chips: impl IntoIterator<Item = impl IntoElement>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(ui::caption(title))
        .child(div().flex().flex_wrap().gap(px(8.)).children(chips))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 四档：由外向内按码率排，环宽随码率，最内一环不小于下限；点在两环之间离谁近算谁，离得远的不算。
    #[test]
    fn rings_follow_bitrate_and_clicks_pick_the_nearest() {
        let rates = [
            (0, Some(1_200_000)),
            (1, Some(18_200_000)),
            (2, Some(6_300_000)),
            (3, Some(3_100_000)),
        ];
        let rings = rings_of(&rates);
        let order: Vec<usize> = rings.iter().map(|r| r.index).collect();
        assert_eq!(order, [1, 2, 3, 0]);
        assert_eq!((rings[0].radius, rings[0].width), (OUTER, 26.));
        assert_eq!(rings[3].radius, INNER);
        assert!(rings.windows(2).all(|pair| pair[0].width > pair[1].width));
        assert_eq!(hit(&rings, OUTER), Some(1));
        assert_eq!(hit(&rings, OUTER + 13. + HIT_SLOP), Some(1));
        assert_eq!(hit(&rings, OUTER + 13. + HIT_SLOP + 1.), None);
        assert_eq!(hit(&rings, INNER - 30.), None);
    }

    /// 档数很多时半径差变小，环宽压到下限，仍从外到内排开、不重叠。
    #[test]
    fn many_variants_still_fit() {
        let rates: Vec<(usize, Option<u64>)> = (0..20)
            .map(|i| (i, Some(1_000_000 * (i as u64 + 1))))
            .collect();
        let rings = rings_of(&rates);
        assert_eq!(rings.len(), 20);
        assert!(rings.iter().all(|r| r.width == 3.));
        assert!(
            rings
                .windows(2)
                .all(|pair| pair[0].radius - pair[1].radius >= pair[0].width)
        );
        let innermost = rings.last().map_or(0., |r| r.radius);
        assert!((innermost - INNER).abs() < 1e-3, "{innermost}");
    }
}
