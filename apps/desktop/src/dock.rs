//! 粘贴栏：舞台底部的链接或 curl 命令的输入。回车或点「探测」发出 [`DockEvent::Probe`]。框里空着或全选时粘贴的 curl
//! 命令不经输入框（单行的框会删掉换行，续行就断了），交给 [`Dock::put`] 并立即发出。外面告诉它正在探测的链接
//! （[`Dock::set_probing`]），框里正是它时右边写探测到哪一步；外面拒绝了输入时右边写原因，改了内容即清掉。任务页、
//! 待机页与新建页的舞台里放的是同一个视图。描边上有一段主色的流光绕着走（[`paint_sweep`]）。

use gpui_kit::component::input::{Input, InputEvent, InputState};
use std::f32::consts::PI;
use std::time::Duration;

use gpui_kit::{
    AppContext as _, Bounds, Context, EventEmitter, Focusable as _, IntoElement,
    ParentElement as _, PathBuilder, Pixels, Point, Render, SharedString, Styled as _,
    Subscription, Window, canvas, div, point, px,
};

use hs_dlx_app::curl;
use hs_m3u8_core::Url;

use crate::links;
use crate::ui::{self, DOCK_HEIGHT, Tone};
use crate::{motion, theme};

/// 胶囊的最大宽。
const MAX_WIDTH: f32 = 760.;
/// 流光绕一圈要多久。
const SWEEP: Duration = Duration::from_secs(6);

pub enum DockEvent {
    /// 框里的链接或命令；框里是记下的 curl 命令的地址时为整条命令（原样，含换行）
    Probe(String),
}

pub struct Dock {
    input: gpui_kit::Entity<InputState>,
    /// 外面拒绝这次输入的原因；改了内容时清掉
    refused: Option<String>,
    /// 显示着的新建页：探测的正是框里的链接时，右边写它探测到哪一步
    probing: Option<(Url, &'static str)>,
    /// 放进来的 curl 命令：框里显示的它的地址与整条命令；改了框里的内容即清掉
    command: Option<(SharedString, String)>,
    _typed: Subscription,
}

impl EventEmitter<DockEvent> for Dock {}

impl Dock {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("粘贴链接或 curl 命令，回车探测"));
        let typed = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, _, cx| match event {
                InputEvent::PressEnter { .. } => cx.emit(DockEvent::Probe(this.submission(cx))),
                InputEvent::Change => {
                    this.refused = None;
                    this.command = None;
                    cx.notify();
                }
                InputEvent::Focus | InputEvent::Blur => cx.notify(),
            },
        );
        Dock {
            input,
            refused: None,
            probing: None,
            command: None,
            _typed: typed,
        }
    }

    /// 回车或点「探测」时发出的：框里是记下的 curl 命令的地址时为整条命令，否则为框里的内容。
    fn submission(&self, cx: &gpui_kit::App) -> String {
        let value = self.text(cx);
        match &self.command {
            Some((shown, command)) if *shown == value => command.clone(),
            _ => value.to_string(),
        }
    }

    /// 把 `text` 放进框里。curl 命令换成它的地址（命令里的凭据不明文显示）并记下整条命令；认不出的命令不放，框里清空、
    /// 右边写原因。别的照原样放。
    pub fn put(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !curl::is_command(text) {
            self.set_text(text.to_owned(), window, cx);
            return;
        }
        match curl::parse(text) {
            Ok(request) => {
                let shown = SharedString::from(request.url.to_string());
                self.set_text(shown.to_string(), window, cx);
                self.command = Some((shown, text.to_owned()));
            }
            Err(error) => {
                self.set_text(String::new(), window, cx);
                self.refuse(error.to_string(), cx);
            }
        }
    }

    pub fn text(&self, cx: &gpui_kit::App) -> SharedString {
        self.input.read(cx).value()
    }

    pub fn set_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.command = None;
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// 键盘焦点在探测框里（用户可能正在输入）。
    pub fn typing(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    /// 外面拒绝了这次输入：右边写 `reason`，改了内容时清掉。
    pub fn refuse(&mut self, reason: String, cx: &mut Context<Self>) {
        self.refused = Some(reason);
        cx.notify();
    }

    pub fn accepted(&mut self, cx: &mut Context<Self>) {
        self.refused = None;
        cx.notify();
    }

    /// 正在探测的链接与探测到哪一步（显示着的新建页报的；没有新建页时为 None）：框里正是这个链接时右边写这一步。
    pub fn set_probing(&mut self, probing: Option<(Url, &'static str)>, cx: &mut Context<Self>) {
        if self.probing != probing {
            self.probing = probing;
            cx.notify();
        }
    }
}

impl Render for Dock {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let value = self.input.read(cx).value();
        let focused = self.typing(window, cx);
        let typed = links::target(&value);
        let probing = self
            .probing
            .as_ref()
            .filter(|(url, _)| typed.as_ref() == Some(url))
            .map(|(_, phase)| *phase);
        let trailing = if let Some(reason) = &self.refused {
            ui::error_text(reason.clone())
                .pr(px(14.))
                .into_any_element()
        } else if let Some(phase) = probing {
            div()
                .pr(px(14.))
                .text_size(px(13.))
                .text_color(theme::muted())
                .child(phase)
                .into_any_element()
        } else {
            // 空着时按了也没东西可探测，不抢眼
            ui::button(
                "probe",
                "探测",
                if value.trim().is_empty() {
                    Tone::Plain
                } else {
                    Tone::Glow
                },
                cx.listener(|this, _, _, cx| {
                    let text = this.submission(cx);
                    cx.emit(DockEvent::Probe(text));
                }),
            )
            .h(px(DOCK_HEIGHT - 14.))
            .rounded_full()
            .into_any_element()
        };
        div()
            .relative()
            .w_full()
            .max_w(px(MAX_WIDTH))
            .h(px(DOCK_HEIGHT))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .pl(px(20.))
            .pr(px(7.))
            .rounded_full()
            .bg(theme::deck())
            .border_1()
            .border_color(if focused {
                theme::accent_dim()
            } else {
                theme::edge()
            })
            .shadow(
                [gpui_kit::BoxShadow {
                    color: theme::alpha(gpui_kit::black(), 0.45),
                    offset: gpui_kit::point(px(0.), px(18.)),
                    blur_radius: px(40.),
                    spread_radius: px(0.),
                    inset: false,
                }]
                .into_iter()
                // 输入时外面一圈淡淡的主色光环
                .chain(focused.then(|| gpui_kit::BoxShadow {
                    color: theme::alpha(theme::accent(), 0.1),
                    offset: gpui_kit::point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(3.),
                    inset: false,
                }))
                .collect(),
            )
            .child(ui::dot(theme::accent(), 8.))
            .child(div().flex_1().min_w_0().text_size(px(14.)).child(
                Input::new(&self.input).appearance(false).on_paste({
                    let dock = cx.entity().downgrade();
                    let input = self.input.clone();
                    move |item, window, cx| {
                        let Some(text) = item
                            .text()
                            .filter(|text| curl::is_command(text) && ui::pastes_over(&input, cx))
                        else {
                            return false;
                        };
                        // 视图已释放时照常粘贴
                        dock.update(cx, |dock, cx| {
                            dock.put(&text, window, cx);
                            if dock.refused.is_none() {
                                cx.emit(DockEvent::Probe(text));
                            }
                        })
                        .is_ok()
                    }
                }),
            ))
            .child(trailing)
            .children(motion::turn(SWEEP).map(|head| {
                canvas(
                    |_, _, _| {},
                    move |bounds, (), window, _| {
                        paint_sweep(bounds, head, window);
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full()
            }))
    }
}

/// 粘贴栏 1 点宽的描边上（`bounds` 为描边以内），沿边走到 `head`（一圈为 1，从上边左端起顺时针）处的一段流光：前后
/// 各约一成周长由暗到亮再到暗，最亮处在 `head`。
fn paint_sweep(bounds: Bounds<Pixels>, head: f32, window: &mut Window) {
    /// 流光在 `head` 前后各占周长的多少，与分几段画。
    const BEFORE: f32 = 0.13;
    const AFTER: f32 = 0.12;
    const STEPS: usize = 48;
    let edge = Stadium::around(bounds);
    let accent = theme::accent();
    for step in 0..STEPS {
        let from = step as f32 / STEPS as f32;
        let to = (step + 1) as f32 / STEPS as f32;
        let middle = (from + to) / 2.;
        // 0 在流光尾端，BEFORE / (BEFORE + AFTER) 处最亮，1 在前端
        let peak = BEFORE / (BEFORE + AFTER);
        let strength = if middle < peak {
            middle / peak
        } else {
            (1. - middle) / (1. - peak)
        };
        // 两端几乎看不见的几段不画
        if strength < 0.1 {
            continue;
        }
        let at = |t: f32| edge.at(head - BEFORE + t * (BEFORE + AFTER));
        for (width, alpha) in [(4., 0.25), (1.5, 0.9)] {
            let mut path = PathBuilder::stroke(px(width));
            path.move_to(at(from));
            path.line_to(at(to));
            let path = path.build().expect("两点的描边，顶点远低于曲面细分的上限");
            window.paint_path(path, theme::alpha(accent, alpha * strength * strength));
        }
    }
}

/// 胶囊形描边的中线：上下两条直边与左右两个半圆。
struct Stadium {
    origin: Point<Pixels>,
    width: f32,
    radius: f32,
}

impl Stadium {
    /// 紧贴 `bounds` 外面的 1 点宽的描边的中线。
    fn around(bounds: Bounds<Pixels>) -> Stadium {
        Stadium {
            origin: point(bounds.origin.x - px(0.5), bounds.origin.y - px(0.5)),
            width: f32::from(bounds.size.width) + 1.,
            radius: (f32::from(bounds.size.height) + 1.) / 2.,
        }
    }

    /// 中线上 `turn`（一圈为 1，从上边左端起顺时针，可以超出 0 到 1）处的点。
    fn at(&self, turn: f32) -> Point<Pixels> {
        let r = self.radius;
        let straight = (self.width - 2. * r).max(0.);
        let perimeter = 2. * straight + 2. * PI * r;
        let arc = PI * r;
        let s = turn.rem_euclid(1.) * perimeter;
        let (x, y) = if s < straight {
            (r + s, 0.)
        } else if s < straight + arc {
            let a = -PI / 2. + (s - straight) / r;
            (r + straight + r * a.cos(), r + r * a.sin())
        } else if s < 2. * straight + arc {
            (r + straight - (s - straight - arc), 2. * r)
        } else {
            let a = PI / 2. + (s - 2. * straight - arc) / r;
            (r + r * a.cos(), r + r * a.sin())
        };
        point(self.origin.x + px(x), self.origin.y + px(y))
    }
}
