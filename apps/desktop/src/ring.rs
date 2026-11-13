//! 环的画法：主角任务的刻度环、待机时的刻度环、列表里的小进度环、新任务的虚线环。都以绘制区域的中心为圆心，从
//! 12 点方向顺时针。

use std::f32::consts::{FRAC_PI_2, TAU};

use gpui_kit::{Bounds, Hsla, PathBuilder, Pixels, Point, Window, point, px};

use crate::theme;

/// 刻度环最多画几个刻度：半径 206 时周长约 1294，每格 2 宽、至少留 1.6 的缝。分片更多时一个刻度管几片。
pub const MAX_TICKS: usize = 360;

/// 刻度的半长：刻度是以环为中线、沿半径方向的短线。
const HALF: f32 = 9.0;
/// 重试中的那一格更长更粗。
const RETRY_HALF: f32 = 13.0;

/// 刻度环的状态：前 `lit` 个刻度已完成，其中前 `written` 个已写出；紧接着的 `active` 个画成下载中，`retrying` 时
/// 其中第一格画成重试色。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ticks {
    pub count: usize,
    pub lit: usize,
    pub written: usize,
    pub active: usize,
    pub retrying: bool,
    /// 已完成刻度的颜色：下载为青色，录制为红色
    pub color: Hsla,
    /// 已完成的刻度发光；暂停、失败时不发光
    pub glow: bool,
}

impl Ticks {
    /// 共 `total` 片、已完成 `done` 片时的刻度：刻度数在 60 到 [`MAX_TICKS`] 之间，一格管 `total / count` 片，按完成
    /// 的比例点亮；`running` 时紧接已完成的一格画成下载中（进度里没有在途的片数，这一格只表示在下）。`total` 为 0（还不
    /// 知道分片数）时一格也不亮。
    pub fn of(total: usize, done: usize, running: bool) -> Ticks {
        let count = total.clamp(60, MAX_TICKS);
        let lit = (done.min(total) * count).checked_div(total).unwrap_or(0);
        Ticks {
            count,
            lit,
            written: 0,
            active: usize::from(running && lit < count),
            retrying: false,
            color: theme::cyan(),
            glow: true,
        }
    }
}

/// 一类刻度共用一条路径：线宽、颜色与半长。
struct Stroke {
    path: PathBuilder,
    color: Hsla,
    half: f32,
}

impl Stroke {
    fn new(width: f32, color: Hsla, half: f32) -> Stroke {
        Stroke {
            path: PathBuilder::stroke(px(width)),
            color,
            half,
        }
    }

    fn tick(&mut self, center: Point<Pixels>, radius: f32, angle: f32) {
        self.path
            .move_to(on_circle(center, radius - self.half, angle));
        self.path
            .line_to(on_circle(center, radius + self.half, angle));
    }

    fn paint(self, window: &mut Window) {
        window.paint_path(self.path.build().expect(TESSELLATED), self.color);
    }
}

/// 在 `bounds` 中心画半径 `radius` 的刻度环。发光是同一组刻度先画两层更粗、更淡的描边。
pub fn paint_ticks(bounds: Bounds<Pixels>, radius: f32, ticks: &Ticks, window: &mut Window) {
    let center = bounds.center();
    let mut idle = Stroke::new(2., theme::line(), HALF);
    let mut halo_wide = Stroke::new(9., theme::alpha(ticks.color, 0.07), HALF + 2.);
    let mut halo_near = Stroke::new(5., theme::alpha(ticks.color, 0.18), HALF + 1.);
    let mut lit = Stroke::new(2., ticks.color, HALF);
    let mut written_halo = Stroke::new(5., theme::alpha(theme::white(), 0.16), HALF + 1.);
    let mut written = Stroke::new(2., theme::white(), HALF);
    let mut active = Stroke::new(2., theme::cyan_dim(), HALF);
    let mut retry_halo = Stroke::new(10., theme::alpha(theme::orange(), 0.3), RETRY_HALF + 2.);
    let mut retry = Stroke::new(3., theme::orange(), RETRY_HALF);
    for i in 0..ticks.count {
        let angle = i as f32 / ticks.count as f32 * TAU - FRAC_PI_2;
        if i < ticks.written {
            written_halo.tick(center, radius, angle);
            written.tick(center, radius, angle);
        } else if i < ticks.lit {
            if ticks.glow {
                halo_wide.tick(center, radius, angle);
                halo_near.tick(center, radius, angle);
            }
            lit.tick(center, radius, angle);
        } else if i == ticks.lit && ticks.retrying {
            retry_halo.tick(center, radius, angle);
            retry.tick(center, radius, angle);
        } else if i < ticks.lit + ticks.active {
            active.tick(center, radius, angle);
        } else {
            idle.tick(center, radius, angle);
        }
    }
    for stroke in [
        idle,
        halo_wide,
        halo_near,
        lit,
        written_halo,
        written,
        active,
        retry_halo,
        retry,
    ] {
        stroke.paint(window);
    }
}

/// 待机时的刻度环：一圈暗刻度，12 点前有一段由淡到亮的彗尾。
pub fn paint_standby(bounds: Bounds<Pixels>, radius: f32, window: &mut Window) {
    const COUNT: usize = 120;
    const TAIL: usize = 22;
    let center = bounds.center();
    let mut idle = Stroke::new(2., theme::line(), HALF);
    for i in 0..COUNT - TAIL {
        idle.tick(center, radius, i as f32 / COUNT as f32 * TAU - FRAC_PI_2);
    }
    idle.paint(window);
    // 越接近 12 点越亮，最前面的一格带光晕
    for k in 0..TAIL {
        let i = COUNT - TAIL + k;
        let angle = i as f32 / COUNT as f32 * TAU - FRAC_PI_2;
        let strength = (k + 1) as f32 / TAIL as f32;
        if k == TAIL - 1 {
            let mut halo = Stroke::new(6., theme::alpha(theme::cyan(), 0.3), HALF + 1.);
            halo.tick(center, radius, angle);
            halo.paint(window);
        }
        let mut tick = Stroke::new(2., theme::alpha(theme::cyan(), strength.powf(1.6)), HALF);
        tick.tick(center, radius, angle);
        tick.paint(window);
    }
}

/// 在 `bounds` 中心画半径 `radius`、宽 `width` 的环：底色 `track` 一整圈，上面从 12 点起顺时针画 `fraction`
/// （0 到 1）的 `color`。
pub fn paint_arc(
    bounds: Bounds<Pixels>,
    radius: f32,
    width: f32,
    fraction: f32,
    color: Hsla,
    track: Hsla,
    window: &mut Window,
) {
    let center = bounds.center();
    for (span, paint) in [(1.0, track), (fraction.clamp(0.0, 1.0), color)] {
        if span > 0.0 {
            window.paint_path(arc(center, radius, width, 0.0, span), paint);
        }
    }
}

/// 虚线圆：`dashes` 段，每段占一格的 60%。
pub fn paint_dashed(
    bounds: Bounds<Pixels>,
    radius: f32,
    width: f32,
    dashes: usize,
    color: Hsla,
    window: &mut Window,
) {
    let center = bounds.center();
    let step = 1.0 / dashes as f32;
    for i in 0..dashes {
        let from = i as f32 * step;
        window.paint_path(arc(center, radius, width, from, step * 0.6), color);
    }
}

/// 从 `from` 起顺时针 `span` 的圆弧（都以整圈为 1），折线逼近。
fn arc(
    center: Point<Pixels>,
    radius: f32,
    width: f32,
    from: f32,
    span: f32,
) -> gpui_kit::Path<Pixels> {
    let steps = ((span * 128.0).ceil() as usize).max(2);
    let mut path = PathBuilder::stroke(px(width));
    for step in 0..=steps {
        let turn = from + step as f32 / steps as f32 * span;
        let at = on_circle(center, radius, turn * TAU - FRAC_PI_2);
        if step == 0 {
            path.move_to(at);
        } else {
            path.line_to(at);
        }
    }
    path.build().expect(TESSELLATED)
}

/// 刻度与圆弧都是不自交的折线，顶点最多几千个，远低于曲面细分的 u16 索引上限，细分不会失败。
const TESSELLATED: &str = "刻度与圆弧的折线曲面细分失败";

fn on_circle(center: Point<Pixels>, radius: f32, angle: f32) -> Point<Pixels> {
    let (sin, cos) = angle.sin_cos();
    point(center.x + px(radius * cos), center.y + px(radius * sin))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_scale_segments_into_the_ring() {
        let big = Ticks::of(664, 412, true);
        assert_eq!((big.count, big.lit, big.active), (360, 223, 1));
        let small = Ticks::of(4, 1, false);
        assert_eq!((small.count, small.lit, small.active), (60, 15, 0));
        let unknown = Ticks::of(0, 0, true);
        assert_eq!((unknown.count, unknown.lit, unknown.active), (60, 0, 1));
        let finished = Ticks::of(160, 160, true);
        assert_eq!((finished.lit, finished.active), (160, 0));
    }
}
