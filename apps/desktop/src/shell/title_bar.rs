//! 顶栏：标志与名字，右边是进行中的任务数、总速度与保存目录所在盘的可用空间。

use std::time::Instant;

use gpui_kit::component::TitleBar;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, canvas, div, px,
};

use super::{Page, Shell};
use crate::tasks::Tasks;
use crate::ui::{self, TITLE_HEIGHT};
use crate::{format, ring, theme};

/// 标志的小环（画到 [`ring::LOGO_ARC`]）的边长与环宽。
const LOGO: f32 = 17.;
const LOGO_WIDTH: f32 = 2.5;
/// 标志离红绿灯多远、与名字的间距、右边状态离窗口边多远。
const LOGO_MARGIN: f32 = 14.;
const LOGO_GAP: f32 = 10.;
const STATUS_MARGIN: f32 = 28.;
/// 可用空间少于这么多时标成橙色：约 10 Mbps 的视频再下十几分钟就满。
const LOW_SPACE: u64 = 1 << 30;
/// 设置按钮的边长。
const SETTINGS: f32 = 36.;

impl Shell {
    pub(super) fn render_header(
        &self,
        tasks: &Tasks,
        now: Instant,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let activity = format::activity(&tasks.activity(now));
        // 可用空间：不足或读不出时标橙色，只标这一段
        let space = tasks.free_space().map(|free| match free {
            Ok(bytes) => (
                format!("可用 {}", format::bytes(*bytes)),
                *bytes < LOW_SPACE,
            ),
            Err(reason) => (reason.clone(), true),
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
                                    ring::LOGO_ARC,
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
            .child(
                div()
                    .pr(px(STATUS_MARGIN))
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(ui::num(
                        div()
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
                    .child(self.settings_button(cx)),
            )
    }

    /// 设置的按钮：齿轮，设置页开着时发光。
    fn settings_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let on = matches!(self.page, Page::Settings { .. });
        div()
            .id("settings")
            .size(px(SETTINGS))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .cursor_pointer()
            .map(|button| {
                if on {
                    button
                        .border_color(theme::edge())
                        .bg(theme::selected())
                        .text_color(theme::cyan())
                        .shadow(ui::glow(theme::cyan(), 14., 0.25))
                } else {
                    button
                        .border_color(gpui_kit::transparent_black())
                        .text_color(theme::muted())
                        .hover(|s| s.bg(theme::raised()))
                }
            })
            .on_click(cx.listener(|this, _, window, cx| this.toggle_settings(window, cx)))
            .child(Icon::new(IconName::Settings).size(px(18.)))
    }
}
