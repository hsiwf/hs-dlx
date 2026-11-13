//! 新建任务：探测地址，每档清晰度画成一个同心环（越粗码率越高，引线连到右侧的标签），点环选中，再选音轨、输出与
//! 保存位置。

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AbsoluteLength, Bounds, Context, Div, EventEmitter, FontWeight, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, PathPromptOptions, Pixels, Point, Render,
    SharedString, StatefulInteractiveElement as _, Styled, Task, Window, canvas, div, px,
};
use hs_m3u8_app::{OutputSpec, Service, StoredPreference, Task as StoredTask, TaskSpec};
use hs_m3u8_core::hls::{AudioChoice, Preference, VariantChoice};
use hs_m3u8_core::{Probe, Source, Url, VariantInfo};

use crate::ui::{self, Tone};
use crate::{format, theme};

/// 新建任务视图发给外面的事件。
pub enum NewTaskEvent {
    /// 已存进存储并开始的任务
    Started(Box<StoredTask>),
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

enum Probing {
    Running,
    Failed(String),
    Ready(Box<Probe>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    Mp4,
    Hls,
    Both,
}

impl Output {
    fn label(self) -> &'static str {
        match self {
            Output::Mp4 => "MP4",
            Output::Hls => "本地 HLS",
            Output::Both => "两者",
        }
    }
}

pub struct NewTask {
    service: Rc<Service>,
    url: Url,
    probing: Probing,
    /// 选中的变体在 `MasterInfo::variants` 里的下标；来源是媒体播放列表时不用
    variant: usize,
    /// 选中的音频在 `MasterInfo::audio` 里的下标；None 为来源的默认
    audio: Option<usize>,
    /// 鼠标所在的环（变体下标）
    hovered: Option<usize>,
    output: Output,
    folder: PathBuf,
    home: Option<PathBuf>,
    /// 同心环的绘制区域，鼠标事件据此算点在哪个环上
    rings_bounds: Rc<Cell<Bounds<Pixels>>>,
    error: Option<String>,
    _probe: Task<()>,
}

impl EventEmitter<NewTaskEvent> for NewTask {}

/// 同心环的区域边长、最外与最内一环的半径。
const BOX: f32 = 480.;
const OUTER: f32 = 222.;
const INNER: f32 = 102.;
/// 标签所在的横坐标（相对同心环区域）：引线从环上 45° 处水平拉到这里。
const LABEL_X: f32 = 496.;

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
            service,
            url,
            probing: Probing::Running,
            variant: 0,
            audio: None,
            hovered: None,
            output: Output::Mp4,
            folder,
            home,
            rings_bounds: Rc::new(Cell::new(Bounds::default())),
            error: None,
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

    /// 窗口坐标 `position` 落在哪一环上：离环宽的边不超过 8 的最近一环。
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
        let distance = (dx * dx + dy * dy).sqrt();
        rings(&master.variants)
            .into_iter()
            .map(|ring| {
                (
                    ring.index,
                    (distance - ring.radius).abs() - ring.width / 2.0,
                )
            })
            .filter(|(_, gap)| *gap <= 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
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

    fn start(&mut self, cx: &mut Context<Self>) {
        let Probing::Ready(probe) = &self.probing else {
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
        let title = title_of(&self.url);
        let stem = free_stem(&self.folder, &title);
        let output = match self.output {
            Output::Mp4 => OutputSpec::Mp4(self.folder.join(format!("{stem}.mp4"))),
            Output::Hls => OutputSpec::Hls(self.folder.join(&stem)),
            Output::Both => OutputSpec::Both {
                mp4: self.folder.join(format!("{stem}.mp4")),
                hls: self.folder.join(&stem),
            },
        };
        let spec = StoredPreference::try_from(&preference).map(|preference| TaskSpec {
            title,
            url: self.url.clone(),
            preference,
            headers: Vec::new(),
            output,
            live: probe.live,
        });
        let Some(created_at) = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
        else {
            self.error = Some("系统时钟早于 1970 年，记不了任务的创建时间".to_owned());
            cx.notify();
            return;
        };
        match spec.and_then(|spec| self.service.create(spec, created_at)) {
            Ok(task) => cx.emit(NewTaskEvent::Started(Box::new(task))),
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            }
        }
    }
}

/// 由外向内的各环：码率从高到低，最外一环半径 [`OUTER`]、最内不小于 [`INNER`]；环宽按码率在 4 到 26 之间，且
/// 与相邻环留出至少 8 的缝。
fn rings(variants: &[VariantInfo]) -> Vec<Ring> {
    let mut order: Vec<&VariantInfo> = variants.iter().collect();
    order.sort_by_key(|v| std::cmp::Reverse(v.bandwidth.unwrap_or(0)));
    let top = order.first().and_then(|v| v.bandwidth).unwrap_or(1).max(1) as f32;
    let step = match order.len() {
        0 | 1 => 0.0,
        n => (48.0_f32).min((OUTER - INNER) / (n - 1) as f32),
    };
    order
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let width = 4.0 + 22.0 * v.bandwidth.unwrap_or(0) as f32 / top;
            let room = if step > 0.0 { step - 8.0 } else { 26.0 };
            Ring {
                index: v.index,
                radius: OUTER - step * i as f32,
                width: width.min(room).max(3.0),
            }
        })
        .collect()
}

/// 列表与文件名用的名字：地址路径的最后一段去掉 `.m3u8`；是 index、master 之类的通用名时取上一级目录名。
fn title_of(url: &Url) -> String {
    const GENERIC: [&str; 5] = ["index", "master", "playlist", "prog_index", "main"];
    let segments: Vec<&str> = url
        .path_segments()
        .map(Iterator::collect)
        .unwrap_or_default();
    let stem = |s: &str| s.strip_suffix(".m3u8").unwrap_or(s).to_owned();
    let last = segments.last().map(|s| stem(s)).unwrap_or_default();
    let name = if GENERIC.contains(&last.to_ascii_lowercase().as_str()) && segments.len() >= 2 {
        segments[segments.len() - 2].to_owned()
    } else {
        last
    };
    let clean: String = name
        .chars()
        .map(|c| {
            if "/\\:*?\"<>|".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim().to_owned();
    if clean.is_empty() {
        url.host_str().unwrap_or("视频").to_owned()
    } else {
        clean
    }
}

/// `folder` 里还没被占用的主名：`name`，或加上 ` 2`、` 3`……；MP4、HLS 目录与任务目录（`<主名>.hsdl`）都算占用。
fn free_stem(folder: &Path, name: &str) -> String {
    let taken = |stem: &str| {
        [
            format!("{stem}.mp4"),
            stem.to_owned(),
            format!("{stem}.hsdl"),
        ]
        .iter()
        .any(|n| folder.join(n).exists())
    };
    if !taken(name) {
        return name.to_owned();
    }
    (2..)
        .map(|n| format!("{name} {n}"))
        .find(|stem| !taken(stem))
        .expect("编号是无穷的，总有没被占用的")
}

/// 一档清晰度的简称：有分辨率时为高度（如 1080p），否则为码率，都没有时为它在主播放列表里的序号。
fn short_label(v: &VariantInfo) -> String {
    match (v.resolution, v.bandwidth) {
        (Some(r), _) => format!("{}p", r.height),
        (None, Some(bw)) => format::bitrate(bw),
        (None, None) => format!("第 {} 档", v.index + 1),
    }
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
            .gap(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .pb(px(16.))
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
                [OUTER, 174., 134.]
                    .into_iter()
                    .map(|r| ring_div(r, 4.).border_color(theme::ring_idle()))
                    .collect::<Vec<_>>(),
                Vec::new(),
                "探测中".to_owned(),
                self.url.host_str().unwrap_or_default().to_owned(),
            ),
            Probing::Failed(_) => (
                vec![ring_div(174., 4.).border_color(theme::orange())],
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
        div().relative().w(px(680.)).h(px(520.)).flex_none().child(
            div()
                .id("rings")
                .absolute()
                .left_0()
                .top(px(20.))
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
                        .left(px(BOX / 2. - 90.))
                        .top(px(BOX / 2. - 90.))
                        .size(px(180.))
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
        let duration = probe.tracks.first().map(|t| t.duration).unwrap_or_default();
        let Some(master) = &probe.master else {
            // 来源本身是媒体播放列表：只有一档，没有可选的
            let segments = probe.tracks.first().map_or(0, |t| t.segments);
            return (
                lit_ring(174., 14.),
                Vec::new(),
                format::clock(duration),
                format!("{segments} 个分片"),
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
                theme::ring_hover()
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
                    format!("{} · {}", short_label(v), format::bitrate(bw))
                }
                _ => short_label(v),
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
                        div().size(px(6.)).rounded_full().bg(theme::ring_hover())
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
            .map(|bw| format!("约 {}", format::bytes(format::estimated_size(bw, duration))))
            .unwrap_or_default();
        (ring_divs, labels, short_label(picked), size)
    }

    fn render_side(&self, cx: &mut Context<Self>) -> Div {
        let host = self.url.host_str().unwrap_or_default().to_owned();
        let mut side = div()
            .w(px(320.))
            .flex_none()
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
                    .child(
                        div()
                            .text_size(px(20.))
                            .line_height(px(28.))
                            .font_weight(FontWeight::BOLD)
                            .child(title_of(&self.url)),
                    )
                    .child(ui::num(ui::caption(self.summary(&host)))),
            );
        match &self.probing {
            Probing::Running => {
                side = side.child(div().flex_1()).child(ui::button(
                    "cancel",
                    "取消",
                    Tone::Ghost,
                    cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed)),
                ));
            }
            Probing::Failed(message) => {
                side = side
                    .child(
                        div()
                            .px(px(14.))
                            .py(px(12.))
                            .rounded(px(12.))
                            .bg(theme::panel())
                            .border_1()
                            .border_color(theme::error_edge())
                            .text_size(px(13.))
                            .line_height(px(19.))
                            .child(SharedString::from(message.clone())),
                    )
                    .child(div().flex_1())
                    .child(ui::button(
                        "dismiss",
                        "返回",
                        Tone::Plain,
                        cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed)),
                    ));
            }
            Probing::Ready(probe) => side = self.ready_side(side, probe, cx),
        }
        side
    }

    /// 标题下的一行：主机 · 时长 · 几档清晰度 · 几条音轨 · 直播。
    fn summary(&self, host: &str) -> String {
        let Probing::Ready(probe) = &self.probing else {
            return match self.probing {
                Probing::Running => format!("{host} · 正在读取播放列表"),
                _ => host.to_owned(),
            };
        };
        let duration = probe.tracks.first().map(|t| t.duration).unwrap_or_default();
        let mut parts = vec![host.to_owned(), format::clock(duration)];
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

    fn ready_side(&self, side: Div, probe: &Probe, cx: &mut Context<Self>) -> Div {
        let master = probe.master.as_ref();
        let duration = probe
            .tracks
            .first()
            .map(|t| t.duration)
            .unwrap_or(Duration::ZERO);
        let audio_options: Vec<(usize, String, bool)> = master
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
                        (a.index, name, on)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let audio_label = audio_options
            .iter()
            .find(|(_, _, on)| *on)
            .map(|(_, name, _)| name.clone());

        let mut side = side;
        if !audio_options.is_empty() {
            let chips = audio_options.into_iter().map(|(index, name, on)| {
                ui::chip(
                    ("audio", index),
                    name,
                    on,
                    cx.listener(move |this, _, _, cx| {
                        this.audio = Some(index);
                        cx.notify();
                    }),
                )
            });
            side = side.child(group("音轨", chips));
        }
        let outputs = [Output::Mp4, Output::Hls, Output::Both].map(|kind| {
            ui::chip(
                kind.label(),
                kind.label(),
                self.output == kind,
                cx.listener(move |this, _, _, cx| {
                    this.output = kind;
                    cx.notify();
                }),
            )
        });
        side = side.child(group("输出", outputs)).child(
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
        if let Some(error) = &self.error {
            side = side.child(
                div()
                    .text_size(px(13.))
                    .text_color(theme::orange())
                    .child(SharedString::from(error.clone())),
            );
        }

        let picked = master.map(|m| &m.variants[self.variant]);
        let headline = picked.map_or_else(
            || "开始下载".to_owned(),
            |v| format!("开始下载 · {}", short_label(v)),
        );
        let detail: Vec<String> = [
            picked
                .and_then(|v| v.bandwidth)
                .map(|bw| format!("约 {}", format::bytes(format::estimated_size(bw, duration)))),
            audio_label,
            Some(self.output.label().to_owned()),
        ]
        .into_iter()
        .flatten()
        .collect();
        side.child(div().flex_1())
            .child(
                div()
                    .id("start")
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
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::cyan_pale()))
                    .on_click(cx.listener(|this, _, _, cx| this.start(cx)))
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

    #[test]
    fn titles_come_from_the_path() {
        let title = |u: &str| title_of(&Url::parse(u).unwrap());
        assert_eq!(title("https://a.example/v/keynote.m3u8?t=1"), "keynote");
        assert_eq!(title("https://a.example/coast/ep3/master.m3u8"), "ep3");
        assert_eq!(title("https://a.example/a:b.m3u8"), "a_b");
        assert_eq!(title("https://a.example/"), "a.example");
    }

    #[test]
    fn stems_skip_names_already_taken() {
        let dir = std::env::temp_dir().join(format!("hs-m3u8-desktop-stem-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(dir.join("talk 2.hsdl")).unwrap();
        assert_eq!(free_stem(&dir, "talk"), "talk");
        std::fs::write(dir.join("talk.mp4"), b"").unwrap();
        assert_eq!(free_stem(&dir, "talk"), "talk 3");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
