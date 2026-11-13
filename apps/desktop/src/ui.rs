//! 界面里反复用到的尺寸与小件：按钮、胶囊、卡片、列表项、小标题、等宽数字、发光。

use std::sync::Arc;

use gpui_kit::{
    App, BoxShadow, ClickEvent, Div, ElementId, FontFallbacks, FontFeatures, FontWeight, Hsla,
    InteractiveElement as _, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, Window, div, point, px,
};

use crate::{new_task, ring, theme};

/// 顶栏高度；macOS 的红绿灯按它垂直居中。
pub const TITLE_HEIGHT: f32 = 56.;
/// 主体左右的内边距。
pub const PAD: f32 = 20.;
/// 各栏之间的间距。
pub const GAP: f32 = 8.;
/// 各视图中间一栏的下边距。
pub const BOTTOM: f32 = 24.;
/// 按钮与胶囊的高度。
pub const BUTTON_HEIGHT: f32 = 38.;
/// 小标题（12 号字）的行高。
pub const CAPTION_LINE: f32 = 18.;
/// 正文（13 号字）的行高。
pub const TEXT_LINE: f32 = 20.;
/// 左边任务列表的宽度。
pub const LIST_WIDTH: f32 = 300.;
/// 窗口窄时任务列表最多收到的宽度。
pub const LIST_MIN_WIDTH: f32 = 240.;
/// 任务视图右边信号栏的宽度。
pub const SIGNAL_WIDTH: f32 = 300.;
/// 窗口窄时信号栏最多收到的宽度。
pub const SIGNAL_MIN_WIDTH: f32 = 240.;
/// 新建页右边选项栏的宽度。
pub const OPTION_WIDTH: f32 = 320.;
/// 窗口窄时选项栏最多收到的宽度：放得下一行「MP4 · 本地 HLS · 两者」三个胶囊。
pub const OPTION_MIN_WIDTH: f32 = 260.;
/// 任务视图里中央刻度环到下面一行按钮的间距。
pub const ACTIONS_GAP: f32 = 24.;
/// 任务视图中间一栏的高：中央刻度环、间距、一行按钮、下边距。
pub const TASK_VIEW_HEIGHT: f32 = ring::AREA + ACTIONS_GAP + BUTTON_HEIGHT + BOTTOM;
/// 待机页里中央刻度环到剪贴板卡片的间距。
pub const SUGGESTION_GAP: f32 = 28.;
/// 剪贴板卡片的上下内边距。
pub const SUGGESTION_PADDING: f32 = 10.;
/// 待机页中间一栏的高：中央刻度环、间距、剪贴板卡片（内边距加按钮与两行字中高的一个）、下边距。
pub const STANDBY_HEIGHT: f32 = ring::AREA
    + SUGGESTION_GAP
    + 2. * SUGGESTION_PADDING
    + max(BUTTON_HEIGHT, CAPTION_LINE + 2. + TEXT_LINE)
    + BOTTOM;
/// 新建页中间一栏的高：同心环区域与下边距。
pub const NEW_TASK_HEIGHT: f32 = new_task::FIGURE_HEIGHT + BOTTOM;
/// 粘贴栏胶囊的高度。
pub const PILL_HEIGHT: f32 = 56.;
/// 粘贴栏的下边距。
pub const FOOTER_BOTTOM: f32 = 24.;
/// 粘贴栏里各行（剪贴板提示、胶囊、输入错误）之间的间距。
pub const FOOTER_GAP: f32 = 10.;
/// 粘贴栏的高：胶囊、下边距，上面一行剪贴板提示（小标题）、下面一行输入错误（正文），各带间距。
pub const FOOTER_HEIGHT: f32 =
    PILL_HEIGHT + FOOTER_BOTTOM + (CAPTION_LINE + FOOTER_GAP) + (TEXT_LINE + FOOTER_GAP);
/// 窗口的最小宽：放得下新建页的三栏（列表与选项栏收到最窄，中间是整个同心环区域）。
pub const MIN_WIDTH: f32 =
    2. * PAD + LIST_MIN_WIDTH + GAP + new_task::FIGURE_WIDTH + GAP + OPTION_MIN_WIDTH;
/// 窗口的最小高：放得下顶栏、最高的中间一栏与粘贴栏。
pub const MIN_HEIGHT: f32 =
    TITLE_HEIGHT + max(max(TASK_VIEW_HEIGHT, STANDBY_HEIGHT), NEW_TASK_HEIGHT) + FOOTER_HEIGHT;

const fn max(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

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

/// 胶囊按钮。
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    tone: Tone,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .h(px(BUTTON_HEIGHT))
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
            .hover(|s| s.border_color(theme::hover_edge()).bg(theme::selected())),
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
        .h(px(BUTTON_HEIGHT))
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
            .hover(|s| s.border_color(theme::hover_edge()))
    }
}

/// 分组的小标题。
pub fn caption(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .line_height(px(CAPTION_LINE))
        .text_color(theme::muted())
        .child(text.into())
}

/// 数字的字体（随应用打包，见 assets/fonts）。
pub const NUMBER_FONT: &str = "Space Grotesk";

/// 数字：用 [`NUMBER_FONT`] 的等宽字形，跳动时不左右晃；同一段里的中文落到系统界面字体。
pub fn num<E: Styled>(mut element: E) -> E {
    element.text_style().font_fallbacks =
        Some(FontFallbacks::from_fonts(vec![".SystemUIFont".to_owned()]));
    element
        .font_family(NUMBER_FONT)
        .font_features(FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
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

/// 卡片：深色底、圆角，描边 `edge` 表示级别（普通、重试、失败）。
pub fn card(edge: Hsla) -> Div {
    div()
        .px(px(14.))
        .py(px(12.))
        .rounded(px(12.))
        .bg(theme::panel())
        .border_1()
        .border_color(edge)
        .text_size(px(13.))
        .line_height(px(19.))
}

/// 任务列表的一项：横排；选中时有底色与描边，否则悬停时有底色。
pub fn list_item(id: impl Into<ElementId>, selected: bool) -> Stateful<Div> {
    let base = div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(14.))
        .p(px(12.))
        .rounded(px(14.))
        .border_1();
    if selected {
        base.border_color(theme::edge()).bg(theme::selected())
    } else {
        base.border_color(gpui_kit::transparent_black())
            .hover(|s| s.bg(theme::raised()))
    }
}

/// 要用户留意的一行字（操作没成、输入不对）。
pub fn error_text(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(13.))
        .line_height(px(TEXT_LINE))
        .text_color(theme::orange())
        .child(text.into())
}
