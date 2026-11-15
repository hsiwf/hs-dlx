//! 粘贴栏：舞台底部的链接输入。回车或点「探测」发出 [`DockEvent::Probe`]；粘贴的是 curl 命令时不放进框里（单行的框
//! 会删掉换行，续行就断了），原样立即发出。外面告诉它正在探测的链接（[`Dock::set_probing`]），框里正是它时右边写
//! 探测到哪一步；外面拒绝了输入（不是链接、命令认不出）时右边写原因，改了内容即清掉。任务页、待机页与新建页的舞台
//! 里放的是同一个视图。

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, EventEmitter, Focusable as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Window, div, px,
};

use hs_dlx_app::curl;
use hs_m3u8_core::Url;

use crate::links;
use crate::theme;
use crate::ui::{self, DOCK_HEIGHT, Tone};

/// 胶囊的最大宽。
const MAX_WIDTH: f32 = 760.;

pub enum DockEvent {
    /// 探测框里的这段文字，或粘贴的 curl 命令（原样，含换行）
    Probe(String),
}

pub struct Dock {
    input: gpui_kit::Entity<InputState>,
    /// 外面拒绝这次输入的原因；改了内容时清掉
    refused: Option<String>,
    /// 显示着的新建页：探测的正是框里的链接时，右边写它探测到哪一步
    probing: Option<(Url, &'static str)>,
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
            |this: &mut Self, input, event: &InputEvent, _, cx| match event {
                InputEvent::PressEnter { .. } => {
                    cx.emit(DockEvent::Probe(input.read(cx).value().to_string()))
                }
                InputEvent::Change => {
                    this.refused = None;
                    cx.notify();
                }
                InputEvent::Focus | InputEvent::Blur => cx.notify(),
            },
        );
        Dock {
            input,
            refused: None,
            probing: None,
            _typed: typed,
        }
    }

    pub fn text(&self, cx: &gpui_kit::App) -> SharedString {
        self.input.read(cx).value()
    }

    pub fn set_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
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
                    let text = this.input.read(cx).value().to_string();
                    cx.emit(DockEvent::Probe(text));
                }),
            )
            .h(px(DOCK_HEIGHT - 14.))
            .rounded_full()
            .into_any_element()
        };
        div()
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
                theme::cyan_dim()
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
                // 输入时外面一圈淡青的光环
                .chain(focused.then(|| gpui_kit::BoxShadow {
                    color: theme::alpha(theme::cyan(), 0.1),
                    offset: gpui_kit::point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(3.),
                    inset: false,
                }))
                .collect(),
            )
            .child(ui::dot(theme::cyan(), 8.))
            .child(div().flex_1().min_w_0().text_size(px(14.)).child(
                Input::new(&self.input).appearance(false).on_paste({
                    let dock = cx.entity().downgrade();
                    move |item, _, cx| {
                        let Some(text) = item.text().filter(|text| curl::is_command(text)) else {
                            return false;
                        };
                        // 视图已释放时照常粘贴
                        dock.update(cx, |_, cx| cx.emit(DockEvent::Probe(text)))
                            .is_ok()
                    }
                }),
            ))
            .child(trailing)
    }
}
