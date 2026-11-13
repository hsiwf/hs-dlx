//! 顶栏：标志与名字，右边是进行中的任务数、总速度与保存目录所在盘的可用空间。

use std::time::Instant;

use gpui_kit::component::TitleBar;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{FontWeight, IntoElement, ParentElement as _, Styled as _, canvas, div, px};
use hs_m3u8_app::Status;

use super::Shell;
use crate::ui::{self, TITLE_HEIGHT};
use crate::{format, ring, theme};

/// 标志：一段四分之三圈的小环。边长、环宽与画到哪里（0 到 1）。
const LOGO: f32 = 17.;
const LOGO_WIDTH: f32 = 2.5;
const LOGO_ARC: f32 = 0.72;
/// 标志离红绿灯多远、与名字的间距、右边状态离窗口边多远。
const LOGO_MARGIN: f32 = 14.;
const LOGO_GAP: f32 = 10.;
const STATUS_MARGIN: f32 = 28.;
/// 可用空间少于这么多时标成橙色：约 10 Mbps 的视频再下十几分钟就满。
const LOW_SPACE: u64 = 1 << 30;

impl Shell {
    pub(super) fn render_header(&self, now: Instant) -> impl IntoElement {
        let running: Vec<_> = self
            .rows
            .iter()
            .filter(|r| r.task.status == Status::Running)
            .collect();
        let activity = (!running.is_empty()).then(|| {
            let speed: f64 = running.iter().map(|r| r.rate.bytes_per_second(now)).sum();
            format!(
                "{} 个进行中 · ↓ {}/s",
                running.len(),
                format::bytes(speed as u64)
            )
        });
        // 可用空间：不足或读不出时标橙色，只标这一段
        let space = self.free_space.as_ref().map(|free| match free {
            Ok(bytes) => (
                format!("可用 {}", format::bytes(*bytes)),
                *bytes < LOW_SPACE,
            ),
            Err(reason) => (format!("可用空间：{reason}"), true),
        });
        TitleBar::new()
            .h(px(TITLE_HEIGHT))
            .bg(theme::ground())
            .border_color(theme::ground())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(LOGO_GAP))
                    .ml(px(LOGO_MARGIN))
                    .child(
                        canvas(
                            |_, _, _| {},
                            |bounds, _, window, _| {
                                ring::paint_arc(
                                    bounds,
                                    LOGO_WIDTH,
                                    LOGO_ARC,
                                    theme::cyan(),
                                    theme::line(),
                                    window,
                                );
                            },
                        )
                        .size(px(LOGO)),
                    )
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::BOLD)
                            .child("hs-m3u8"),
                    ),
            )
            .child(ui::num(
                div()
                    .pr(px(STATUS_MARGIN))
                    .flex()
                    .gap(px(6.))
                    .text_size(px(13.))
                    .text_color(theme::muted())
                    .children(activity)
                    .children(space.map(|(text, warn)| {
                        div()
                            .when(warn, |s| s.text_color(theme::orange()))
                            .child(text)
                    })),
            ))
    }
}
