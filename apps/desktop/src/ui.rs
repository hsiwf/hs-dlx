//! 界面里反复用到的尺寸与小件：面板、读数格、主操作、按钮、选项、卡片、步骤、列表项、小标题、等宽数字、发光。
//!
//! 版式：面板（[`deck`]）收住空白；当前对象的操作在检查面板（[`inspector`]）的底栏，主操作（[`primary`]）在前；
//! 整页面板的操作在它标题栏（[`page_head`]）的右侧。

use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, ClickEvent, Div, ElementId, FontFallbacks, FontFeatures,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled, Window, div, point, px,
};

use crate::theme;

/// 顶栏高度；macOS 的红绿灯按它垂直居中，右上角的提示从它下面开始。
pub const TITLE_HEIGHT: f32 = 52.;
/// 主体左右与底部的内边距。
pub const PAD: f32 = 14.;
/// 各栏之间的间距。
pub const GAP: f32 = 14.;
/// 面板的圆角。
pub const DECK_RADIUS: f32 = 20.;
/// 检查面板的宽。
pub const INSPECTOR_WIDTH: f32 = 344.;
/// 窗口窄时检查面板最多收到的宽。
pub const INSPECTOR_MIN_WIDTH: f32 = 296.;
/// 粘贴栏的高。
pub const DOCK_HEIGHT: f32 = 52.;
/// 舞台里表盘与下面（提示、粘贴栏）的最小间距。
pub const DOCK_GAP: f32 = 18.;
/// 粘贴栏离舞台底边多远。
pub const DOCK_BOTTOM: f32 = 4.;
/// 按钮的高度。
pub const BUTTON_HEIGHT: f32 = 40.;
/// 输入框与只读的一格（[`field`]）的高度。
pub const FIELD_HEIGHT: f32 = 38.;
/// 选项（[`chip`]）的高度。
pub const CHIP_HEIGHT: f32 = 34.;
/// 小标题（12 号字）的行高。
pub const CAPTION_LINE: f32 = 18.;
/// 正文（13 号字）的行高。
pub const TEXT_LINE: f32 = 20.;
/// 两个尺寸里大的一个（常量表达式里用）。
pub const fn max(a: f32, b: f32) -> f32 {
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
    /// 橙色实心：要处理时最该按的那一个
    Amber,
    /// 只有文字：退出当前流程
    Ghost,
}

/// 实心按钮的底色、悬停时的底色与上面的字色；描边与只有文字的为 None。
fn solid(tone: Tone) -> Option<(Hsla, Hsla, Hsla)> {
    match tone {
        Tone::Glow => Some((theme::cyan(), theme::cyan_pale(), theme::on_cyan())),
        Tone::Rec => Some((
            theme::red(),
            theme::alpha(theme::red(), 0.85),
            gpui_kit::white(),
        )),
        Tone::Amber => Some((
            theme::orange(),
            theme::alpha(theme::orange(), 0.85),
            theme::on_amber(),
        )),
        Tone::Plain | Tone::Ghost => None,
    }
}

/// 按钮。
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    tone: Tone,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .h(px(BUTTON_HEIGHT))
        .px(px(18.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(12.))
        .text_size(px(14.))
        .cursor_pointer()
        .on_click(on_click)
        .child(label.into());
    if let Some((bg, hover, text)) = solid(tone) {
        return base
            .bg(bg)
            .text_color(text)
            .font_weight(FontWeight::BOLD)
            .shadow(glow(bg, 18., 0.3))
            .hover(move |s| s.bg(hover));
    }
    match tone {
        Tone::Ghost => base
            .text_color(theme::muted())
            .hover(|s| s.text_color(theme::text()).bg(theme::selected())),
        _ => base
            .border_1()
            .border_color(theme::edge())
            .bg(theme::raised())
            .text_color(theme::text())
            .hover(|s| s.border_color(theme::hover_edge()).bg(theme::selected())),
    }
}

/// 当前对象的主操作：整宽、两行（做什么与它的后果，如「开始下载 · 1080p」「约 1.9 GB · MP4」），放在检查面板底栏的
/// 最前面。`tone` 为实心的三种之一；`enabled` 为 false 时变淡、不可点。
pub fn primary(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
    tone: Tone,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (bg, hover, text) = solid(tone).expect("主操作是实心的");
    let base = div()
        .id(id)
        .w_full()
        .flex()
        .flex_col()
        .gap(px(1.))
        .px(px(16.))
        .py(px(11.))
        .rounded(px(14.))
        .bg(bg)
        .text_color(text)
        // 光往下投，不漫进上面的正文
        .shadow(vec![BoxShadow {
            color: theme::alpha(bg, 0.22),
            offset: point(px(0.), px(10.)),
            blur_radius: px(26.),
            spread_radius: px(-6.),
            inset: false,
        }])
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::BOLD)
                .child(title.into()),
        )
        .children(detail.map(|detail| num(div().text_size(px(12.)).opacity(0.75).child(detail))));
    if enabled {
        base.cursor_pointer()
            .on_click(on_click)
            .hover(move |s| s.bg(hover))
    } else {
        base.opacity(0.55)
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
        .h(px(CHIP_HEIGHT))
        .px(px(13.))
        .flex()
        .items_center()
        .rounded(px(10.))
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
        base.border_color(theme::edge())
            .text_color(theme::soft())
            .hover(|s| s.border_color(theme::hover_edge()))
    }
}

/// 分组的小标题。
pub fn caption(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .line_height(px(CAPTION_LINE))
        .text_color(theme::faint())
        .child(text.into())
}

/// 面板：任务栏、检查面板与整页面板的外框（深一档的底、细边、大圆角）。
pub fn deck() -> Div {
    div()
        .flex()
        .flex_col()
        .min_h_0()
        .overflow_hidden()
        .rounded(px(DECK_RADIUS))
        .bg(theme::deck())
        .border_1()
        .border_color(theme::deck_edge())
}

/// 检查面板：标题栏（`title` 与下面一行 `sub`）、可以滚动的正文 `body`、底栏 `foot`（当前对象的操作）。
pub fn inspector(
    title: impl Into<SharedString>,
    sub: impl Into<SharedString>,
    body: impl IntoIterator<Item = AnyElement>,
    foot: impl IntoIterator<Item = AnyElement>,
) -> Div {
    let foot: Vec<AnyElement> = foot.into_iter().collect();
    deck()
        .flex_basis(px(INSPECTOR_WIDTH))
        .flex_shrink(1.)
        .min_w(px(INSPECTOR_MIN_WIDTH))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(5.))
                .px(px(22.))
                .pt(px(22.))
                .pb(px(18.))
                .border_b_1()
                .border_color(theme::hairline())
                .child(
                    div()
                        .text_size(px(19.))
                        .line_height(px(25.))
                        .font_weight(FontWeight::BOLD)
                        .line_clamp(2)
                        .child(title.into()),
                )
                .child(
                    num(div()
                        .text_size(px(13.))
                        .text_color(theme::muted())
                        .truncate())
                    .child(sub.into()),
                ),
        )
        .child(
            div()
                .id("inspector-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(20.))
                .px(px(22.))
                .py(px(18.))
                .children(body),
        )
        .when(!foot.is_empty(), |inspector| {
            inspector.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(16.))
                    .pt(px(14.))
                    .pb(px(16.))
                    .border_t_1()
                    .border_color(theme::hairline())
                    .children(foot),
            )
        })
}

/// 整页面板的标题栏：左边 `lead`（图形）、标题 `title` 与下面一行 `sub`，右边 `actions`（这一页的操作）。
pub fn page_head(
    lead: Option<AnyElement>,
    title: impl Into<SharedString>,
    sub: Option<SharedString>,
    actions: impl IntoIterator<Item = AnyElement>,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(14.))
        .pl(px(20.))
        .pr(px(16.))
        .py(px(14.))
        .border_b_1()
        .border_color(theme::hairline())
        .children(lead)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(17.))
                        .font_weight(FontWeight::BOLD)
                        .truncate()
                        .child(title.into()),
                )
                .children(sub.map(|sub| {
                    num(div()
                        .text_size(px(13.))
                        .text_color(theme::muted())
                        .truncate())
                    .child(sub)
                })),
        )
        .children(actions)
}

/// 读数格：两列，每格上面是小标签、下面是读数（等宽数字；读数为文字时如「直连」照样放）。`wide` 为占满一行的一格
/// （长的读数，如代理地址），放在最后；`cells` 为奇数个时最后一格也占满一行，不留空格。
pub fn reads(
    cells: Vec<(SharedString, SharedString)>,
    wide: Option<(SharedString, SharedString)>,
) -> Div {
    let odd = cells.len() % 2 == 1;
    let last = cells.len().saturating_sub(1);
    let cell = |label: SharedString, value: SharedString, span: bool, size: f32| {
        div()
            .min_w_0()
            .when(span, |cell| cell.col_span(2))
            .flex()
            .flex_col()
            .gap(px(3.))
            .px(px(14.))
            .py(px(11.))
            .bg(theme::cell())
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme::faint())
                    .child(label),
            )
            .child(num(div().text_size(px(size)).truncate()).child(value))
    };
    div()
        .grid()
        .grid_cols(2)
        .gap(px(1.))
        .rounded(px(14.))
        .overflow_hidden()
        .bg(theme::deck_edge())
        .border_1()
        .border_color(theme::deck_edge())
        .children(
            cells
                .into_iter()
                .enumerate()
                .map(|(i, (label, value))| cell(label, value, odd && i == last, 16.)),
        )
        .children(wide.map(|(label, value)| cell(label, value, true, 14.)))
}

/// 一步的状态。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Done,
    /// 正在做
    Now,
    /// 在这一步没成
    Failed,
    Waiting,
}

/// 按先后的几步：做完的实心点，正在做的带光圈，没成的橙色。
pub fn steps(items: impl IntoIterator<Item = (SharedString, Step)>) -> Div {
    div()
        .flex()
        .flex_col()
        .children(items.into_iter().map(|(text, step)| {
            let marker = div().size(px(10.)).flex_none().rounded_full().border_2();
            let marker = match step {
                Step::Done => marker.bg(theme::cyan()).border_color(theme::cyan()),
                Step::Now => {
                    marker
                        .border_color(theme::cyan())
                        .shadow(glow(theme::cyan(), 8., 0.5))
                }
                Step::Failed => {
                    marker
                        .border_color(theme::orange())
                        .shadow(glow(theme::orange(), 8., 0.5))
                }
                Step::Waiting => marker.border_color(theme::edge()),
            };
            let color = match step {
                Step::Done => theme::soft(),
                Step::Now => theme::text(),
                Step::Failed => theme::orange(),
                Step::Waiting => theme::muted(),
            };
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .py(px(6.))
                .child(div().w(px(14.)).flex().justify_center().child(marker))
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(color)
                        .when(matches!(step, Step::Now | Step::Failed), |s| {
                            s.font_weight(FontWeight::SEMIBOLD)
                        })
                        .child(text),
                )
        }))
}

/// 只读的一格（如保存位置）：与输入框同样的外观，放不下时截断。
pub fn field(text: impl Into<SharedString>) -> Div {
    div()
        .min_w_0()
        .h(px(FIELD_HEIGHT))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(10.))
        .bg(theme::ground())
        .border_1()
        .border_color(theme::ring_idle())
        .text_size(px(13.))
        .text_color(theme::soft())
        .child(num(div().min_w_0().truncate()).child(text.into()))
}

/// 面板里的一段说明：`warn` 时为要处理的（橙色描边）。
pub fn note(text: impl Into<SharedString>, warn: bool) -> Div {
    div()
        .px(px(14.))
        .py(px(12.))
        .rounded(px(14.))
        .border_1()
        .border_color(if warn {
            theme::warn_edge()
        } else {
            theme::deck_edge()
        })
        .bg(if warn {
            theme::warn_ground()
        } else {
            theme::cell()
        })
        .text_size(px(13.))
        .line_height(px(20.))
        .text_color(theme::soft())
        .child(text.into())
}

/// 状态胶囊：发光的小圆点与一句状态（标题栏里用）。
pub fn status(color: Hsla, text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .h(px(28.))
        .px(px(11.))
        .rounded_full()
        .bg(theme::raised())
        .border_1()
        .border_color(theme::deck_edge())
        .text_size(px(12.))
        .text_color(theme::soft())
        .child(dot(color, 7.))
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

/// 卡片：面板的底色、圆角，描边 `edge` 表示级别（普通、要留意）；浮在内容上的提示用。
pub fn card(edge: Hsla) -> Div {
    div()
        .px(px(14.))
        .py(px(12.))
        .rounded(px(14.))
        .bg(theme::deck())
        .shadow(vec![BoxShadow {
            color: theme::alpha(gpui_kit::black(), 0.5),
            offset: point(px(0.), px(12.)),
            blur_radius: px(32.),
            spread_radius: px(0.),
            inset: false,
        }])
        .border_1()
        .border_color(edge)
        .text_size(px(13.))
        .line_height(px(19.))
}

/// 舞台：两块面板之间、直接在舱底上的一栏。`figure`（表盘、同心环）在剩下的高里居中，下面依次是 `below`（如剪贴板
/// 里的链接）与粘贴栏 `dock`。`min_width` 为放得下 `figure` 的宽：窗口窄时先收两侧的面板。
pub fn stage(
    min_width: f32,
    figure: impl IntoElement,
    below: impl IntoIterator<Item = AnyElement>,
    dock: impl IntoElement,
) -> Div {
    div()
        .flex_1()
        .min_w(px(min_width))
        .min_h_0()
        // 表盘的背光与待机的刻度面不画出舞台
        .overflow_hidden()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(12.))
        .pb(px(DOCK_BOTTOM))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .pb(px(DOCK_GAP - 12.))
                .child(figure),
        )
        .children(below)
        .child(dock)
}

/// 舞台最少要多高：`figure` 高 `figure_height` 时，加上与粘贴栏的间距、粘贴栏与它的下边距。
pub const fn stage_height(figure_height: f32) -> f32 {
    figure_height + DOCK_GAP + DOCK_HEIGHT + DOCK_BOTTOM
}

/// 任务栏的一项：横排；选中时有底色与描边，否则悬停时有底色。
pub fn list_item(id: impl Into<ElementId>, selected: bool) -> Stateful<Div> {
    let base = div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(12.))
        .p(px(10.))
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

/// 两个同心实线环（外环宽 `width`、亮色，内环小 `inset`、细而暗），边长 `size`：表示一个可以探测出几档清晰度的
/// 地址，嗅探页里的流与任务列表里回到嗅探页的一项用它。
pub fn rings(size: f32, width: f32, inset: f32) -> Div {
    let ring = |ring: f32, width: f32, color| {
        div()
            .absolute()
            .left(px((size - ring) / 2.))
            .top(px((size - ring) / 2.))
            .size(px(ring))
            .rounded_full()
            .border(px(width))
            .border_color(color)
    };
    div()
        .relative()
        .size(px(size))
        .flex_none()
        .child(ring(size, width, theme::cyan()))
        .child(ring(size - inset, 2., theme::cyan_dim()))
}
