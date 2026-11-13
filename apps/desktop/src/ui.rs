//! 界面里反复用到的小件：按钮、胶囊、小标题、等宽数字、发光。

use std::sync::Arc;

use gpui_kit::{
    App, BoxShadow, ClickEvent, Div, ElementId, FontFeatures, FontWeight, Hsla,
    InteractiveElement as _, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, Window, div, point, px,
};

use crate::theme;

/// 按钮的样子。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// 描边，深色底
    Plain,
    /// 发光的青色实心：每屏最该按的那一个
    Glow,
    /// 红色实心：停止录制
    Rec,
    /// 只有文字：退出当前流程
    Ghost,
}

/// 38 高的胶囊按钮。
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    tone: Tone,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .h(px(38.))
        .px(px(16.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_size(px(13.))
        .cursor_pointer()
        .on_click(on_click)
        .child(label.into());
    match tone {
        Tone::Plain => base
            .border_1()
            .border_color(theme::stroke())
            .bg(theme::raised())
            .text_color(theme::text())
            .hover(|s| s.border_color(theme::ring_hover()).bg(theme::selected())),
        Tone::Glow => base
            .bg(theme::cyan())
            .text_color(theme::on_cyan())
            .font_weight(FontWeight::BOLD)
            .shadow(glow(theme::cyan(), 18., 0.35))
            .hover(|s| s.bg(theme::cyan_pale())),
        Tone::Rec => base
            .bg(theme::red())
            .text_color(gpui_kit::white())
            .font_weight(FontWeight::BOLD)
            .shadow(glow(theme::red(), 18., 0.35)),
        Tone::Ghost => base
            .text_color(theme::muted())
            .hover(|s| s.text_color(theme::text())),
    }
}

/// 可选中的胶囊：选中时实心浅色。
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .h(px(38.))
        .px(px(14.))
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .text_size(px(13.))
        .cursor_pointer()
        .on_click(on_click)
        .child(label.into());
    if selected {
        base.bg(theme::text())
            .border_color(theme::text())
            .text_color(theme::ground())
            .font_weight(FontWeight::SEMIBOLD)
    } else {
        base.border_color(theme::stroke())
            .text_color(theme::soft())
            .hover(|s| s.border_color(theme::ring_hover()))
    }
}

/// 分组的小标题。
pub fn caption(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .text_color(theme::muted())
        .child(text.into())
}

/// 数字用等宽字形，跳动时不左右晃。
pub fn num<E: Styled>(element: E) -> E {
    element.font_features(FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
}

/// `color` 的外发光：模糊半径 `blur`、透明度 `strength`（0 到 1）。
pub fn glow(color: Hsla, blur: f32, strength: f32) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: theme::alpha(color, strength),
        offset: point(px(0.), px(0.)),
        blur_radius: px(blur),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// 发光的小圆点，`size` 为直径。
pub fn dot(color: Hsla, size: f32) -> Div {
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(color)
        .shadow(glow(color, size, 0.9))
}
