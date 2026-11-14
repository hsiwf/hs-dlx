//! 粘贴栏：舞台底部的链接输入。回车或点「探测」发出 [`DockEvent::Probe`]；探测着的正是框里的链接时，右边写新建页
//! 探测到哪一步；外面拒绝了输入（不是链接）时右边写原因，改了内容即清掉。任务页、待机页与新建页的舞台里各放一份同
//! 一个视图。

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, EventEmitter, Focusable as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, WeakEntity, Window, div, px,
};

use crate::links::http_url;
use crate::new_task::NewTask;
use crate::theme;
use crate::ui::{self, DOCK_HEIGHT, Tone};

/// 胶囊的最大宽。
const MAX_WIDTH: f32 = 760.;

pub enum DockEvent {
    /// 探测框里（或剪贴板里）的这段文字
    Probe(String),
}

pub struct Dock {
    input: gpui_kit::Entity<InputState>,
    /// 外面拒绝这次输入的原因；改了内容时清掉
    refused: Option<String>,
    /// 显示着的新建页：探测的正是框里的链接时，右边写它探测到哪一步
    new_task: Option<WeakEntity<NewTask>>,
    _typed: Subscription,
}

impl EventEmitter<DockEvent> for Dock {}

impl Dock {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("粘贴链接，回车探测"));
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
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        Dock {
            input,
            refused: None,
            new_task: None,
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

    /// 显示着的新建页换了（没有新建页时为 None）。
    pub fn set_new_task(&mut self, view: Option<WeakEntity<NewTask>>, cx: &mut Context<Self>) {
        self.new_task = view;
        cx.notify();
    }
}

impl Render for Dock {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let value = self.input.read(cx).value();
        let typed = http_url(&value);
        let probing = self
            .new_task
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .and_then(|view| {
                let view = view.read(cx);
                (typed.as_ref() == Some(view.url())).then(|| view.phase().label())
            });
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
            .border_color(theme::edge())
            .shadow(vec![gpui_kit::BoxShadow {
                color: theme::alpha(gpui_kit::black(), 0.45),
                offset: gpui_kit::point(px(0.), px(18.)),
                blur_radius: px(40.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(ui::dot(theme::cyan(), 8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(14.))
                    .child(Input::new(&self.input).appearance(false)),
            )
            .child(trailing)
    }
}
