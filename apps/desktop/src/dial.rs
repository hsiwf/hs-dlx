//! 中央表盘的版式：背光、表圈、撑满 [`AREA`] 的刻度（任务的分片、直播钟面、探测的彗尾）、深色圆盘与圆盘里的字。
//! 任务页、待机页、新建页（探测中与失败）与嗅探页（找流中）共用。

use std::time::Duration;

use gpui_kit::{
    AnyElement, Div, Hsla, IntoElement, ParentElement as _, Styled as _, canvas, div, px,
};

use crate::ring::{self, AREA};
use crate::{theme, ui};

/// 圆盘离绘制区域边缘多远。
const DISC_INSET: f32 = 70.;
/// 圆盘里的字离圆盘边多远。
const DISC_PADDING: f32 = 24.;
/// 彗尾（[`ring::paint_comet`]）转一圈的秒数。
pub const TURN: f32 = 4.;
/// 彗尾动画一帧的间隔：彗尾的位置按格取整（一圈 120 格），一帧正好前进一格，更密的帧画出来也一样。
pub const FRAME: Duration = Duration::from_micros(33_333);

/// 圆盘里一行说明最宽多少：两行时仍在圆盘里。
pub const TEXT_WIDTH: f32 = 240.;
/// 背光：表盘后面一个看不见的圆投下的大片柔光，直径与模糊半径。
const LIGHT: f32 = 300.;
const LIGHT_BLUR: f32 = 200.;
/// 背光的强度（0 到 1）：只在舱底上透出一层颜色，不抢刻度。
const LIGHT_STRENGTH: f32 = 0.09;

/// 表盘：`light` 为背光的颜色（随状态），`figure` 为撑满整块区域的刻度，圆盘压在中间，`center` 放在圆盘里。
pub fn dial(light: Hsla, figure: AnyElement, center: impl IntoElement) -> Div {
    let disc = AREA - 2. * DISC_INSET;
    let place = || {
        div()
            .absolute()
            .top(px(DISC_INSET))
            .left(px(DISC_INSET))
            .size(px(disc))
    };
    face(light)
        .child(figure)
        .child(
            place()
                .rounded_full()
                .bg(theme::disc())
                .border_1()
                .border_color(theme::hairline()),
        )
        .child(
            place()
                .px(px(DISC_PADDING))
                .flex()
                .items_center()
                .justify_center()
                .child(center),
        )
}

/// 没有圆盘的表盘（直播钟面的指针扫过圆心）：背光与表圈，`figure` 与其余的字由调用方叠上去。
pub fn face(light: Hsla) -> Div {
    let offset = (AREA - LIGHT) / 2.;
    div()
        .relative()
        .size(px(AREA))
        .flex_none()
        .child(
            div()
                .absolute()
                .top(px(offset))
                .left(px(offset))
                .size(px(LIGHT))
                .rounded_full()
                .shadow(ui::glow(light, LIGHT_BLUR, LIGHT_STRENGTH)),
        )
        .child(
            canvas(
                |_, _, _| {},
                |bounds, _, window, _| ring::paint_bezel(bounds, window),
            )
            .absolute()
            .size_full(),
        )
}

/// 撑满表盘的刻度画布：`paint` 在整块区域上画。
pub fn figure(
    paint: impl FnOnce(gpui_kit::Bounds<gpui_kit::Pixels>, &mut gpui_kit::Window, &mut gpui_kit::App)
    + 'static,
) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, cx| paint(bounds, window, cx),
    )
    .absolute()
    .size_full()
    .into_any_element()
}
