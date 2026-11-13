//! 直播钟面：一圈一小时，每格 15 秒。内圈是正在录的这一小时，外圈是上一小时；录到的格子按会话交替着色，缺片处是
//! 加长的橙色格；录制中有一根红色指针指着录到的位置。时刻都按已录时长（第 0 条轨已完成分片的声明时长之和）算。

use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::Duration;

use gpui_kit::{Bounds, Hsla, Pixels, Point, Window, fill, point, px, size};
use hs_m3u8_core::Progress;

use crate::ring::{Stroke, on_circle};
use crate::theme;

/// 一圈的时长。
pub const LAP: Duration = Duration::from_secs(3600);
/// 一圈的格数：每格 15 秒。
pub const TICKS: usize = 240;
/// 外圈与内圈的半径（绘制区域边长为 [`crate::ring::AREA`]）。
pub const OUTER: f32 = 181.;
pub const INNER: f32 = 153.;
/// 刻度的半长。
const HALF: f32 = 7.;
/// 缺片格的半长：比普通格长，一眼能看到。
const MISS_HALF: f32 = 10.;
/// 圆心红点的半径。
const HUB: f32 = 7.;

/// 这次打开应用以来看到的录制时间线：会话从哪里起、哪里缺了片。只在内存里，重开应用后从下一次进度重新记。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Timeline {
    /// 会话的起点（已录时长）与会话编号，按先后；第一项的起点为 0（之前录的都算作它）
    pub sessions: Vec<(Duration, u32)>,
    /// 缺片：记下时的已录时长与片数，按先后
    pub misses: Vec<(Duration, usize)>,
    /// 上一次进度里本次运行缺的片数（取不到的加滑出窗口的），算增量用；新的运行从 0 起
    missed: usize,
}

impl Timeline {
    /// 记下一次直播进度：会话编号变了即另起一段，缺的片数多了即在当时的位置记一处缺片。
    pub fn observe(&mut self, progress: &Progress) {
        if let Some(session) = progress.session {
            match self.sessions.last() {
                None => self.sessions.push((Duration::ZERO, session)),
                Some(&(_, current)) if current != session => {
                    self.sessions.push((progress.duration, session));
                }
                Some(_) => {}
            }
        }
        let missed = progress.segments.failed + progress.segments.expired;
        // 少了说明是新的运行（核心按本次运行计）
        if missed > self.missed {
            self.misses.push((progress.duration, missed - self.missed));
        }
        self.missed = missed;
    }

    /// 已录时长 `at` 处的会话在 [`Timeline::sessions`] 里的下标；还没见过会话时为 0。
    pub fn session_index(&self, at: Duration) -> usize {
        self.sessions
            .iter()
            .rposition(|&(start, _)| start <= at)
            .unwrap_or(0)
    }

    /// 已录时长 `at` 处的会话起点与编号；还没见过会话时为 None。
    pub fn session_at(&self, at: Duration) -> Option<(Duration, u32)> {
        self.sessions.get(self.session_index(at)).copied()
    }
}

/// 一圈里的位置：已录时长 `at` 落在第几圈、圈里的比例（0 到 1，从 12 点顺时针）。
pub fn lap_of(at: Duration) -> (u64, f32) {
    let lap = at.as_secs() / LAP.as_secs();
    let within = at.saturating_sub(LAP * u32::try_from(lap).unwrap_or(u32::MAX));
    (lap, within.as_secs_f32() / LAP.as_secs_f32())
}

/// 钟面的状态。
pub struct Face<'a> {
    pub recorded: Duration,
    pub timeline: &'a Timeline,
    /// 正在录：画指针，录到的格子发光
    pub recording: bool,
}

/// 第 `lap` 圈画在半径 `radius` 的环上，录到 `recorded` 为止；`lap` 为 None 时整圈是空格（还没有上一小时）。
fn paint_lap(
    center: Point<Pixels>,
    radius: f32,
    lap: Option<u64>,
    face: &Face<'_>,
    window: &mut Window,
) {
    let mut idle = Stroke::new(2., theme::line(), HALF);
    let palette = [theme::cyan(), theme::written()];
    let mut lit: Vec<Stroke> = palette
        .iter()
        .map(|&color| Stroke::new(2., color, HALF))
        .collect();
    let mut halos: Vec<Stroke> = palette
        .iter()
        .map(|&color| Stroke::new(6., theme::alpha(color, 0.16), HALF + 1.))
        .collect();
    let start = lap.map(|lap| LAP * u32::try_from(lap).unwrap_or(u32::MAX));
    for i in 0..TICKS {
        let angle = i as f32 / TICKS as f32 * TAU - FRAC_PI_2;
        let at = start.map(|start| start + LAP / TICKS as u32 * i as u32);
        match at {
            Some(at) if at < face.recorded => {
                let k = face.timeline.session_index(at) % palette.len();
                if face.recording {
                    halos[k].tick(center, radius, angle);
                }
                lit[k].tick(center, radius, angle);
            }
            _ => idle.tick(center, radius, angle),
        }
    }
    idle.paint(window);
    for stroke in halos.into_iter().chain(lit) {
        stroke.paint(window);
    }
    if let Some(start) = start {
        let mut halo = Stroke::new(8., theme::alpha(theme::orange(), 0.35), MISS_HALF + 1.);
        let mut miss = Stroke::new(3., theme::orange(), MISS_HALF);
        for &(at, _) in &face.timeline.misses {
            if at >= start && at < start + LAP {
                let angle = lap_of(at).1 * TAU - FRAC_PI_2;
                halo.tick(center, radius, angle);
                miss.tick(center, radius, angle);
            }
        }
        halo.paint(window);
        miss.paint(window);
    }
}

/// 在 `bounds` 中心画钟面：内圈是已录时长所在的这一圈，外圈是上一圈；录制中再画指针与圆心红点。
pub fn paint(bounds: Bounds<Pixels>, face: &Face<'_>, window: &mut Window) {
    let center = bounds.center();
    let (lap, within) = lap_of(face.recorded);
    paint_lap(center, OUTER, lap.checked_sub(1), face, window);
    paint_lap(center, INNER, Some(lap), face, window);
    if !face.recording {
        return;
    }
    let angle = within * TAU - FRAC_PI_2;
    let tip = on_circle(center, INNER - HALF - 4., angle);
    for (width, color) in [(8., theme::alpha(theme::red(), 0.25)), (2., theme::red())] {
        let mut hand = gpui_kit::PathBuilder::stroke(px(width));
        hand.move_to(center);
        hand.line_to(tip);
        window.paint_path(hand.build().expect(crate::ring::TESSELLATED), color);
    }
    let hub = Bounds::new(
        point(center.x - px(HUB), center.y - px(HUB)),
        size(px(2. * HUB), px(2. * HUB)),
    );
    window.paint_quad(fill(hub, theme::red()).corner_radii(px(HUB)));
}

/// 钟面四个刻度标记（00、15、30、45 分）相对圆心的位置：半径 `radius`、从 12 点顺时针。
pub fn marks(radius: f32) -> [(&'static str, f32, f32); 4] {
    let at = |turn: f32| {
        let angle = turn * TAU - FRAC_PI_2;
        (radius * angle.cos(), radius * angle.sin())
    };
    let [a, b, c, d] = [0., 0.25, 0.5, 0.75].map(at);
    [
        ("00", a.0, a.1),
        ("15", b.0, b.1),
        ("30", c.0, c.1),
        ("45", d.0, d.1),
    ]
}

/// 颜色在钟面图例里的说法：会话下标为 `index` 的格子用的颜色。
pub fn session_color(index: usize) -> Hsla {
    [theme::cyan(), theme::written()][index % 2]
}

#[cfg(test)]
mod tests {
    use hs_m3u8_core::SegmentCounts;

    use super::*;

    fn progress(session: u32, minutes: u64, failed: usize, expired: usize) -> Progress {
        Progress {
            session: Some(session),
            duration: Duration::from_secs(minutes * 60),
            segments: SegmentCounts {
                failed,
                expired,
                ..SegmentCounts::default()
            },
            ..Progress::default()
        }
    }

    /// 会话变了另起一段；缺的片数多了在当时的位置记一处，新的运行（计数归零）从 0 起重新算增量。
    #[test]
    fn the_timeline_follows_sessions_and_misses() {
        let mut timeline = Timeline::default();
        timeline.observe(&progress(0, 0, 0, 0));
        timeline.observe(&progress(0, 30, 0, 0));
        timeline.observe(&progress(1, 41, 0, 3));
        timeline.observe(&progress(1, 50, 1, 3));
        timeline.observe(&progress(1, 60, 0, 0));
        timeline.observe(&progress(1, 61, 0, 2));
        let minutes = |m: u64| Duration::from_secs(m * 60);
        assert_eq!(timeline.sessions, [(minutes(0), 0), (minutes(41), 1)]);
        assert_eq!(
            timeline.misses,
            [(minutes(41), 3), (minutes(50), 1), (minutes(61), 2)]
        );
        assert_eq!(timeline.session_index(minutes(40)), 0);
        assert_eq!(timeline.session_at(minutes(45)), Some((minutes(41), 1)));
    }

    #[test]
    fn positions_wrap_every_hour() {
        assert_eq!(lap_of(Duration::ZERO), (0, 0.0));
        assert_eq!(lap_of(Duration::from_secs(15 * 60)), (0, 0.25));
        let (lap, within) = lap_of(Duration::from_secs(3600 + 45 * 60));
        assert_eq!(lap, 1);
        assert!((within - 0.75).abs() < 1e-6, "{within}");
    }
}
