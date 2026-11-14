//! 环的画法：中央的刻度环（任务的分片、待机与探测的彗尾）、外圈的表圈与列表里的小进度环；刻度、角度与圆上的点也给直播钟面（[`crate::live`]）
//! 用。都以绘制区域的中心为圆心，从 12 点方向顺时针。

use std::f32::consts::{FRAC_PI_2, TAU};

use gpui_kit::{Bounds, Hsla, PathBuilder, Pixels, Point, Window, point, px};

use crate::theme;

/// 中央刻度环的绘制区域边长与刻度环的半径。
pub const AREA: f32 = 460.;
pub const RADIUS: f32 = 206.;

/// 表圈（[`paint_bezel`]）刻度的外沿半径：离绘制区域的边留 1。
const BEZEL: f32 = AREA / 2. - 1.;

/// 刻度环最多画几个刻度：在 [`RADIUS`] 的环上每格 2 宽、至少留 1.6 的缝。分片更多时一个刻度管几片。
pub const MAX_TICKS: usize = 360;

/// 应用的标志是一段画到这里（0 到 1）的环：顶栏与托盘用。
pub const LOGO_ARC: f32 = 0.72;

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
pub(crate) struct Stroke {
    path: PathBuilder,
    color: Hsla,
    half: f32,
}

impl Stroke {
    pub(crate) fn new(width: f32, color: Hsla, half: f32) -> Stroke {
        Stroke {
            path: PathBuilder::stroke(px(width)),
            color,
            half,
        }
    }

    /// 以半径 `radius` 的环为中线、在角度 `angle`（弧度，0 为 3 点方向）处加一格。
    pub(crate) fn tick(&mut self, center: Point<Pixels>, radius: f32, angle: f32) {
        self.path
            .move_to(on_circle(center, radius - self.half, angle));
        self.path
            .line_to(on_circle(center, radius + self.half, angle));
    }

    pub(crate) fn paint(self, window: &mut Window) {
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
    let mut written_halo = Stroke::new(5., theme::alpha(theme::written(), 0.16), HALF + 1.);
    let mut written = Stroke::new(2., theme::written(), HALF);
    let mut active = Stroke::new(2., theme::cyan_dim(), HALF);
    let mut retry_halo = Stroke::new(10., theme::alpha(theme::orange(), 0.3), RETRY_HALF + 2.);
    let mut retry = Stroke::new(3., theme::orange(), RETRY_HALF);
    for i in 0..ticks.count {
        let rad = angle(i as f32 / ticks.count as f32);
        if i < ticks.written {
            written_halo.tick(center, radius, rad);
            written.tick(center, radius, rad);
        } else if i < ticks.lit {
            if ticks.glow {
                halo_wide.tick(center, radius, rad);
                halo_near.tick(center, radius, rad);
            }
            lit.tick(center, radius, rad);
        } else if i == ticks.lit && ticks.retrying {
            retry_halo.tick(center, radius, rad);
            retry.tick(center, radius, rad);
        } else if i < ticks.lit + ticks.active {
            active.tick(center, radius, rad);
        } else {
            idle.tick(center, radius, rad);
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

/// 彗尾环：一圈 120 格暗刻度，`head`（整圈为 1，从 12 点顺时针）前的 22 格由淡到亮成 `color`，最前面的一格带光晕。
/// 待机时彗头停在 12 点；探测、找流时随时间转。
pub fn paint_comet(
    bounds: Bounds<Pixels>,
    radius: f32,
    head: f32,
    color: Hsla,
    window: &mut Window,
) {
    const COUNT: usize = 120;
    const TAIL: usize = 22;
    let center = bounds.center();
    let first = (head.rem_euclid(1.) * COUNT as f32).round() as usize + COUNT - TAIL;
    let lit = |i: usize| (i + COUNT - first % COUNT) % COUNT;
    let mut idle = Stroke::new(2., theme::line(), HALF);
    for i in (0..COUNT).filter(|&i| lit(i) >= TAIL) {
        idle.tick(center, radius, angle(i as f32 / COUNT as f32));
    }
    idle.paint(window);
    for k in 0..TAIL {
        let rad = angle(((first + k) % COUNT) as f32 / COUNT as f32);
        let strength = (k + 1) as f32 / TAIL as f32;
        if k == TAIL - 1 {
            let mut halo = Stroke::new(6., theme::alpha(color, 0.3), HALF + 1.);
            halo.tick(center, radius, rad);
            halo.paint(window);
        }
        let mut tick = Stroke::new(2., theme::alpha(color, strength.powf(1.6)), HALF);
        tick.tick(center, radius, rad);
        tick.paint(window);
    }
}

/// 表圈：贴着 [`AREA`] 边的 120 格细刻度，每 10 格一格长而亮，只作刻度盘的外框、不表示数据。
pub fn paint_bezel(bounds: Bounds<Pixels>, window: &mut Window) {
    const COUNT: usize = 120;
    let center = bounds.center();
    let mut major = Stroke::new(1.2, theme::stroke(), 3.5);
    let mut minor = Stroke::new(1.2, theme::hairline(), 2.);
    for i in 0..COUNT {
        let rad = angle(i as f32 / COUNT as f32);
        if i % 10 == 0 {
            major.tick(center, BEZEL - 3.5, rad);
        } else {
            minor.tick(center, BEZEL - 2., rad);
        }
    }
    major.paint(window);
    minor.paint(window);
}

/// 待机时舞台的刻度面，以 `bounds` 的中心（表盘的圆心）为圆心：表盘外一圈细实线、一圈短划、一圈点，两条贯穿的准线
/// 与准线上表盘外的短刻度（每 5 格一格长）。线会画出 `bounds`，由舞台裁掉；都很淡，只给舞台一层仪表的质感。
pub fn paint_scope(bounds: Bounds<Pixels>, window: &mut Window) {
    const SOLID: f32 = 300.;
    const DASHED: f32 = 350.;
    const DOTTED: f32 = 420.;
    const REACH: f32 = 1600.;
    const STEP: f32 = 24.;
    let center = bounds.center();
    window.paint_path(arc(center, SOLID, 1., 0., 1.), theme::hairline());
    let mut dashes = PathBuilder::stroke(px(1.));
    for i in 0..120 {
        let from = i as f32 / 120.;
        dashes.move_to(on_circle(center, DASHED, angle(from)));
        dashes.line_to(on_circle(center, DASHED, angle(from + 1. / 360.)));
    }
    window.paint_path(
        dashes.build().expect(TESSELLATED),
        theme::alpha(theme::hover_edge(), 0.3),
    );
    let mut dots = Stroke::new(1.2, theme::alpha(theme::hover_edge(), 0.25), 0.6);
    for i in 0..240 {
        dots.tick(center, DOTTED, angle(i as f32 / 240.));
    }
    dots.paint(window);
    let mut cross = PathBuilder::stroke(px(1.));
    for (dx, dy) in [(1., 0.), (0., 1.)] {
        cross.move_to(point(center.x - px(REACH * dx), center.y - px(REACH * dy)));
        cross.line_to(point(center.x + px(REACH * dx), center.y + px(REACH * dy)));
    }
    window.paint_path(cross.build().expect(TESSELLATED), theme::hairline());
    let mut marks = PathBuilder::stroke(px(1.));
    let first = (AREA / 2. / STEP).ceil() as usize;
    for k in first..(REACH / STEP) as usize {
        let half = if k % 5 == 0 { 6. } else { 3. };
        let at = k as f32 * STEP;
        for sign in [-1., 1.] {
            let along = px(sign * at);
            marks.move_to(point(center.x + along, center.y - px(half)));
            marks.line_to(point(center.x + along, center.y + px(half)));
            marks.move_to(point(center.x - px(half), center.y + along));
            marks.line_to(point(center.x + px(half), center.y + along));
        }
    }
    window.paint_path(
        marks.build().expect(TESSELLATED),
        theme::alpha(theme::hover_edge(), 0.45),
    );
}

/// 在 `bounds` 里画一个贴着边、宽 `width` 的环：底色 `track` 一整圈，上面从 12 点起顺时针画 `fraction`（0 到 1）
/// 的 `color`。
pub fn paint_arc(
    bounds: Bounds<Pixels>,
    width: f32,
    fraction: f32,
    color: Hsla,
    track: Hsla,
    window: &mut Window,
) {
    let center = bounds.center();
    let side = f32::from(bounds.size.width.min(bounds.size.height));
    let radius = side / 2. - width / 2.;
    for (span, paint) in [(1.0, track), (fraction.clamp(0.0, 1.0), color)] {
        if span > 0.0 {
            window.paint_path(arc(center, radius, width, 0.0, span), paint);
        }
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
        let at = on_circle(center, radius, angle(turn));
        if step == 0 {
            path.move_to(at);
        } else {
            path.line_to(at);
        }
    }
    path.build().expect(TESSELLATED)
}

/// 刻度、圆弧与钟面指针都是不自交的折线，顶点最多几千个，远低于曲面细分的 u16 索引上限，细分不会失败。
pub(crate) const TESSELLATED: &str = "环上的折线曲面细分失败";

/// 整圈为 1 的比例 `turn`（从 12 点顺时针）换成弧度（0 为 3 点方向）。
pub(crate) fn angle(turn: f32) -> f32 {
    turn * TAU - FRAC_PI_2
}

pub(crate) fn on_circle(center: Point<Pixels>, radius: f32, angle: f32) -> Point<Pixels> {
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
