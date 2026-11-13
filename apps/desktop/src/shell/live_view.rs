//! 直播录制的任务视图：中间是钟面（见 [`crate::live`]），信号栏上面是录制的概况与图例。

use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Div, FontWeight, Hsla, ParentElement as _, SharedString, Styled as _, canvas, div, px,
};

use super::row::{Look, Row};
use crate::live::{self, Face};
use crate::ring::AREA;
use crate::ui::{self, CAPTION_LINE, TEXT_LINE};
use crate::{format, theme};

/// 钟面上 00、15、30、45 分标记的半径。
const MARK_RADIUS: f32 = 210.;
/// 圆心下方文字一栏的上边距（相对圆心）。
const READOUT_TOP: f32 = 18.;
/// 圆心下方文字一栏的宽：连同上下 4 的内边距，整栏落在内圈刻度里面（底边距圆心 110，那里刻度内沿的半宽约 96），
/// 垫着的底色不挡刻度。
const READOUT_WIDTH: f32 = 168.;
/// 最近一处缺片或另起会话的说明离外圈的距离。
const NOTE_GAP: f32 = 14.;

/// 钟面：刻度、标记、圆心下方的状态与已录时长、最近一处缺片或另起会话的说明。
pub(super) fn figure(row: &Row, look: &Look, recorded: Duration, recording: bool) -> Div {
    let timeline = row.timeline.clone();
    let painted = timeline.clone();
    let half = AREA / 2.;
    let marks = live::marks(MARK_RADIUS).map(|(text, x, y)| {
        div()
            .absolute()
            .left(px(half + x - 12.))
            .top(px(half + y - CAPTION_LINE / 2.))
            .w(px(24.))
            .text_center()
            .child(ui::num(ui::caption(text)))
    });
    let session = timeline.session_at(recorded).map(|(start, session)| {
        if start.is_zero() {
            format!("会话 {}", session + 1)
        } else {
            format!("会话 {} · 自 {}", session + 1, format::clock(start))
        }
    });
    div()
        .relative()
        .size(px(AREA))
        .flex_none()
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let face = Face {
                        recorded,
                        timeline: &painted,
                        recording,
                    };
                    live::paint(bounds, &face, window);
                },
            )
            .absolute()
            .size_full(),
        )
        .children(marks)
        .child(
            div()
                .absolute()
                .left(px(half - READOUT_WIDTH / 2.))
                .top(px(half + READOUT_TOP))
                .w(px(READOUT_WIDTH))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.))
                // 与窗口同色的底：指针扫到下半圈时从字后面过，不压字
                .py(px(4.))
                .rounded(px(12.))
                .bg(theme::ground())
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(CAPTION_LINE))
                        .font_weight(FontWeight::BOLD)
                        .text_color(look.color)
                        .child(look.state.clone()),
                )
                .child(ui::num(
                    div()
                        .text_size(px(36.))
                        .line_height(px(40.))
                        .font_weight(FontWeight::BOLD)
                        .child(look.big.clone()),
                ))
                .child(ui::num(
                    div()
                        .text_size(px(12.))
                        .line_height(px(CAPTION_LINE))
                        .text_color(theme::muted())
                        .text_center()
                        .child(SharedString::from(
                            session.unwrap_or_else(|| look.line.clone()),
                        )),
                )),
        )
        .children(note(&timeline, recorded))
}

/// 最近一处缺片或另起会话（只看钟面上画着的这两圈）的说明，贴在它在外圈上的位置旁边。
fn note(timeline: &live::Timeline, recorded: Duration) -> Option<Div> {
    let (lap, _) = live::lap_of(recorded);
    let shown_from = live::LAP * u32::try_from(lap.saturating_sub(1)).unwrap_or(u32::MAX);
    let missed = timeline
        .misses
        .iter()
        .map(|&(at, count)| (at, format!("{} · 缺 {count} 片", format::clock(at))));
    let restarted = timeline.sessions.iter().skip(1).map(|&(at, session)| {
        (
            at,
            format!("{} · 另起会话 {}", format::clock(at), session + 1),
        )
    });
    let (at, text) = missed
        .chain(restarted)
        .filter(|&(at, _)| at >= shown_from)
        .max_by_key(|&(at, _)| at)?;
    let angle = live::lap_of(at).1 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
    let radius = live::OUTER + NOTE_GAP;
    let half = AREA / 2.;
    let (x, y) = (half + radius * angle.cos(), half + radius * angle.sin());
    let pill = div()
        .absolute()
        .top(px(y - TEXT_LINE / 2. - 4.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(8.))
        .bg(theme::panel())
        .border_1()
        .border_color(theme::warn_edge())
        .text_size(px(12.))
        .line_height(px(TEXT_LINE))
        .text_color(theme::orange())
        .whitespace_nowrap()
        .child(ui::num(div().child(text)));
    // 在圆心右边的往右伸，左边的往左伸，不压住钟面
    Some(if x >= half {
        pill.left(px(x))
    } else {
        pill.right(px(AREA - x))
    })
}

/// 信号栏上面的录制概况：标题与来源，已录、缺失、会话、刷新，与钟面的图例。
pub(super) fn summary(row: &Row) -> Div {
    let host = row.task.spec.url.host_str().unwrap_or_default().to_owned();
    let recorded = row.completed().map_or_else(
        || "—".to_owned(),
        |(segments, bytes)| format!("{segments} 片 · {}", format::bytes(bytes)),
    );
    // 刷新只在录制中有意义；停下的不沿用最后一次进度里的
    let progress = row
        .progress
        .as_ref()
        .filter(|_| row.task.status == hs_m3u8_app::Status::Running);
    let missed: usize = row.timeline.misses.iter().map(|&(_, count)| count).sum();
    let sessions = row.timeline.sessions.len();
    let refresh = progress
        .and_then(|p| p.refresh_errors.iter().flatten().next())
        .map_or_else(
            || if progress.is_some() { "正常" } else { "—" }.to_owned(),
            |reason| format!("失败：{reason}"),
        );
    let refresh_failed = progress.is_some_and(|p| p.refresh_errors.iter().any(Option::is_some));
    let stat = |label: &'static str, value: String| {
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .min_w_0()
            .child(ui::caption(label))
            .child(ui::num(
                div()
                    .text_size(px(13.))
                    .line_height(px(TEXT_LINE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(value),
            ))
    };
    // 只有一个会话时不提会话；有几个时按钟面上的两种颜色列出各自的会话
    let sessions_legend: Vec<(Hsla, String)> = match row.timeline.sessions.as_slice() {
        [] | [_] => vec![(live::session_color(0), "录到的".to_owned())],
        all => (0..2)
            .map(|k| {
                let numbers: Vec<String> = all
                    .iter()
                    .skip(k)
                    .step_by(2)
                    .map(|&(_, session)| (session + 1).to_string())
                    .collect();
                (
                    live::session_color(k),
                    format!("会话 {}", numbers.join("、")),
                )
            })
            .collect(),
    };
    let legend = |color: Hsla, half: f32, text: String| {
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(12.))
            .line_height(px(CAPTION_LINE))
            .child(div().w(px(2.)).h(px(2. * half)).rounded(px(1.)).bg(color))
            .child(text)
    };
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(18.))
                        .font_weight(FontWeight::BOLD)
                        .line_clamp(2)
                        .child(row.task.spec.title.clone()),
                )
                .child(ui::caption(host)),
        )
        .child(
            div()
                .grid()
                .grid_cols(2)
                .gap(px(12.))
                .child(stat("已录", recorded))
                .child(stat(
                    "缺失",
                    if missed == 0 {
                        "无".to_owned()
                    } else {
                        format!("{missed} 片")
                    },
                ))
                .child(stat(
                    "会话",
                    if sessions == 0 {
                        "—".to_owned()
                    } else {
                        sessions.to_string()
                    },
                ))
                .child(
                    stat("刷新", refresh).when(refresh_failed, |s| s.text_color(theme::orange())),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .children(
                    sessions_legend
                        .into_iter()
                        .map(|(color, text)| legend(color, 8., text)),
                )
                .child(legend(
                    theme::orange(),
                    10.,
                    "缺片（取不到或刷新前已滑出窗口）".to_owned(),
                )),
        )
}
