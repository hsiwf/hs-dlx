//! 选中任务的视图：中间的刻度环与下面的操作，右边的信号；没有任务时的待机页。

use std::time::Instant;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, canvas, div, px,
};
use hs_m3u8_app::Status;
use hs_m3u8_core::bare_url;

use super::plan::{Action, Plan, stopped_plan};
use super::row::{Row, look};
use super::{Shell, live_view};
use crate::ring;
use crate::signals::{Level, Signal};
use crate::theme;
use crate::ui::{self, BOTTOM, BUTTON_HEIGHT, CAPTION_LINE, GAP, TEXT_LINE, Tone, max};

/// 中央刻度环里的深色圆盘离绘制区域边缘多远。
const DISC_INSET: f32 = 70.;
/// 圆盘里的字离圆盘边多远。
const DISC_PADDING: f32 = 24.;
/// 右边信号栏的宽度。
const SIGNAL_WIDTH: f32 = 300.;
/// 窗口窄时信号栏最多收到的宽度。
const SIGNAL_MIN_WIDTH: f32 = 240.;
/// 中央刻度环到下面一行按钮的间距。
const ACTIONS_GAP: f32 = 24.;
/// 待机页里中央刻度环到剪贴板卡片的间距。
const SUGGESTION_GAP: f32 = 28.;
/// 剪贴板卡片的上下内边距。
const SUGGESTION_PADDING: f32 = 10.;
/// 剪贴板卡片里两行字的间距。
const SUGGESTION_LINE_GAP: f32 = 2.;
/// 任务视图的最小宽：中央刻度环与收到最窄的信号栏。
pub(super) const MIN_WIDTH: f32 = ring::AREA + GAP + SIGNAL_MIN_WIDTH;
/// 任务视图中间一栏的最小高：中央刻度环、间距、一行按钮、下边距。
pub(super) const MIN_HEIGHT: f32 = ring::AREA + ACTIONS_GAP + BUTTON_HEIGHT + BOTTOM;
/// 待机页的最小高：中央刻度环、间距、剪贴板卡片（内边距、1 像素描边，与按钮和两行字中高的一个）、下边距。
pub(super) const STANDBY_MIN_HEIGHT: f32 = ring::AREA
    + SUGGESTION_GAP
    + 2. * (SUGGESTION_PADDING + 1.)
    + max(
        BUTTON_HEIGHT,
        CAPTION_LINE + SUGGESTION_LINE_GAP + TEXT_LINE,
    )
    + BOTTOM;

impl Shell {
    /// 中间一栏与右边的信号栏。
    pub(super) fn render_task(
        &self,
        row: &Row,
        now: Instant,
        cx: &mut Context<Self>,
    ) -> (Div, Div) {
        let look = look(row, now);
        let ticks = look.ticks;
        // 直播录制（没完成的）画钟面
        let live = row.recording() && row.task.status != Status::Done;
        let ring = if live {
            live_view::figure(row, &look)
        } else {
            ring_with_disc(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        ring::paint_ticks(bounds, ring::RADIUS, &ticks, window)
                    },
                )
                .absolute()
                .size_full()
                .into_any_element(),
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(look.color)
                            .child(look.state),
                    )
                    .child(ui::num(
                        div()
                            .text_size(px(64.))
                            .line_height(px(64.))
                            .font_weight(FontWeight::BOLD)
                            .child(look.big),
                    ))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_center()
                            .line_clamp(2)
                            .child(row.task.spec.title.clone()),
                    )
                    .child(ui::num(
                        div()
                            .text_size(px(12.))
                            .text_color(theme::muted())
                            .text_center()
                            .line_clamp(3)
                            .child(SharedString::from(look.line)),
                    )),
            )
        };
        // 中间一栏至少放得下中央刻度环：窗口窄时先收两侧的栏
        let main = div()
            .flex_1()
            .min_w(px(ring::AREA))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(ACTIONS_GAP))
            .pb(px(BOTTOM))
            .child(ring)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(10.))
                    .children(self.actions(row, cx)),
            );
        // 最新的在最上面，多了可以滚动
        let aside = div()
            .flex_basis(px(SIGNAL_WIDTH))
            .flex_shrink(1.)
            .min_w(px(SIGNAL_MIN_WIDTH))
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .py(px(8.))
            .pr(px(8.))
            .children(live.then(|| live_view::summary(row)))
            .children(self.headers_card(row, cx))
            .child(ui::caption("信号"))
            .child(
                div()
                    .id("signals")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .pb(px(16.))
                    .children(row.signals.iter().rev().map(signal_card)),
            );
        (main, aside)
    }

    /// 正在改这个任务的请求头时，信号栏上方的编辑卡片。
    fn headers_card(&self, row: &Row, cx: &mut Context<Self>) -> Option<Div> {
        let (_, editor) = self
            .editing_headers
            .as_ref()
            .filter(|(id, _)| *id == row.task.id)?;
        let resume = self.plan(row).resume_after_headers;
        let label = match (resume, &row.task.status) {
            (false, _) => "保存",
            (true, _) if row.task.write_only => "保存并续录",
            (true, Status::Failed(_)) => "保存并重试",
            (true, _) => "保存并继续",
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .p(px(14.))
                .rounded(px(14.))
                .bg(theme::panel())
                .border_1()
                .border_color(theme::edge())
                .child(ui::caption("请求头"))
                .child(editor.clone())
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(ui::button(
                            "save-headers",
                            label,
                            Tone::Glow,
                            cx.listener(move |this, _, window, cx| {
                                this.save_headers(resume, window, cx)
                            }),
                        ))
                        .child(ui::button(
                            "cancel-headers",
                            "取消",
                            Tone::Ghost,
                            cx.listener(|this, _, _, cx| {
                                this.editing_headers = None;
                                cx.notify();
                            }),
                        )),
                ),
        )
    }

    /// 停着的任务 `row` 的操作（见 [`stopped_plan`]）。
    fn plan(&self, row: &Row) -> Plan {
        let failure = match &row.task.status {
            Status::Failed(failure) => Some(failure.kind),
            _ => None,
        };
        let editing = self
            .editing_headers
            .as_ref()
            .is_some_and(|(at, _)| *at == row.task.id);
        stopped_plan(failure, row.recording(), row.task.write_only, editing)
    }

    /// 暂停或失败的任务的操作（见 [`stopped_plan`]），第一个发光，改请求头不在第一个时只是文字。
    fn stopped_actions(
        &self,
        row: &Row,
        discard: AnyElement,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let id = row.task.id;
        let failure = match &row.task.status {
            Status::Failed(failure) => Some(failure.kind),
            _ => None,
        };
        let mut actions: Vec<AnyElement> = self
            .plan(row)
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let tone = match (index, action) {
                    (0, _) => Tone::Glow,
                    (_, Action::EditHeaders) => Tone::Ghost,
                    _ => Tone::Plain,
                };
                match action {
                    Action::EditHeaders => ui::button(
                        "edit-headers",
                        "改请求头",
                        tone,
                        cx.listener(move |this, _, window, cx| this.edit_headers(id, window, cx)),
                    ),
                    Action::WriteRecorded => ui::button(
                        "write-recorded",
                        "写出已录的",
                        tone,
                        cx.listener(move |this, _, _, cx| this.write_recorded(id, cx)),
                    ),
                    Action::SwitchToHls => ui::button(
                        "switch-hls",
                        "改存本地 HLS",
                        tone,
                        cx.listener(move |this, _, _, cx| this.switch_to_hls(id, cx)),
                    ),
                    Action::Resume => ui::button(
                        "resume",
                        match (row.task.write_only, failure) {
                            (true, _) => "续录",
                            (false, Some(_)) => "重试",
                            (false, None) => "继续",
                        },
                        tone,
                        cx.listener(move |this, _, _, cx| this.resume(id, cx)),
                    ),
                    Action::OpenFolder => {
                        let target = row.task.spec.output.options().target;
                        let folder = target
                            .mp4()
                            .or(target.hls())
                            .and_then(|path| path.parent())
                            .expect("输出是以文件名结尾的绝对路径")
                            .to_path_buf();
                        ui::button(
                            "open-folder",
                            "打开保存目录",
                            tone,
                            move |_, _, cx| cx.open_with_system(&folder),
                        )
                    }
                }
                .into_any_element()
            })
            .collect();
        actions.push(discard);
        actions
    }

    fn actions(&self, row: &Row, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if !row.idle() {
            return Vec::new();
        }
        let id = row.task.id;
        let confirming = self.confirm_discard == Some(id);
        let discard = |cx: &mut Context<Self>| {
            ui::button(
                "discard",
                if confirming {
                    "再点一次：删除已下的内容"
                } else {
                    "放弃任务"
                },
                Tone::Plain,
                cx.listener(move |this, _, _, cx| this.discard(id, confirming, cx)),
            )
            .when(confirming, |b| {
                b.border_color(theme::orange()).text_color(theme::orange())
            })
            .into_any_element()
        };
        let pause = |cx: &mut Context<Self>| {
            ui::button(
                "pause",
                "暂停",
                Tone::Plain,
                cx.listener(move |this, _, _, _| this.service.pause(id)),
            )
            .into_any_element()
        };
        match &row.task.status {
            Status::DiscardUnfinished => vec![discard(cx)],
            Status::Running if row.stoppable() => vec![
                ui::button(
                    "stop",
                    "停止并写出",
                    Tone::Rec,
                    cx.listener(move |this, _, _, cx| {
                        this.service.stop(id);
                        cx.notify();
                    }),
                )
                .into_any_element(),
                pause(cx),
                discard(cx),
            ],
            Status::Running => vec![pause(cx), discard(cx)],
            // 没有自己的任务目录（与别的任务共用、迁移时让了出去）：不能继续，放弃会删别人的，只能移除记录
            Status::Paused | Status::Failed(_) if row.task.work_dir.is_none() => vec![
                ui::button(
                    "forget",
                    "从列表移除",
                    Tone::Plain,
                    cx.listener(move |this, _, _, cx| this.forget(id, cx)),
                )
                .into_any_element(),
            ],
            Status::Paused | Status::Failed(_) => self.stopped_actions(row, discard(cx), cx),
            Status::Done => {
                // 输出在不在是完成时与打开应用时在后台查的；还没查完时先不给这两个按钮
                let (mp4, shown) = row
                    .present
                    .as_ref()
                    .map_or((None, None), |p| (p.mp4.clone(), p.shown.clone()));
                let mut actions = Vec::new();
                if let Some(mp4) = mp4 {
                    actions.push(
                        ui::button("play", "▶ 播放", Tone::Glow, move |_, _, cx| {
                            cx.open_with_system(&mp4)
                        })
                        .into_any_element(),
                    );
                }
                if let Some(shown) = shown {
                    actions.push(
                        ui::button(
                            "reveal",
                            "在访达中显示",
                            Tone::Plain,
                            move |_, _, cx| cx.reveal_path(&shown),
                        )
                        .into_any_element(),
                    );
                }
                actions.push(
                    ui::button(
                        "forget",
                        "从列表移除",
                        Tone::Ghost,
                        cx.listener(move |this, _, _, cx| this.forget(id, cx)),
                    )
                    .into_any_element(),
                );
                actions
            }
        }
    }

    /// 没有任务也没在新建时：待机的刻度环；剪贴板里有链接时放在环下面，一键探测。
    pub(super) fn render_standby(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ring = ring_with_disc(
            canvas(
                |_, _, _| {},
                |bounds, _, window, _| ring::paint_standby(bounds, ring::RADIUS, window),
            )
            .absolute()
            .size_full()
            .into_any_element(),
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::cyan())
                        .child("就绪"),
                )
                .child(
                    div()
                        .text_size(px(30.))
                        .font_weight(FontWeight::BOLD)
                        .child("粘贴链接"),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme::muted())
                        .child("m3u8 · 回车探测"),
                ),
        );
        let suggestion = self.clipboard.clone().map(|url| {
            let shown = bare_url(&url).to_string();
            div()
                .flex()
                .items_center()
                .gap(px(14.))
                .w_full()
                .max_w(px(560.))
                .pl(px(20.))
                .pr(px(10.))
                .py(px(SUGGESTION_PADDING))
                .rounded(px(18.))
                .bg(theme::panel())
                .border_1()
                .border_color(theme::edge())
                .child(ui::dot(theme::cyan(), 8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(SUGGESTION_LINE_GAP))
                        .child(ui::caption("剪贴板里的链接"))
                        .child(
                            div()
                                .text_size(px(13.))
                                .line_height(px(TEXT_LINE))
                                .truncate()
                                .child(shown),
                        ),
                )
                .child(ui::button(
                    "probe-clipboard",
                    "探测",
                    Tone::Glow,
                    cx.listener(move |this, _, window, cx| this.probe_url(&url, window, cx)),
                ))
        });
        div()
            .flex_1()
            .min_w(px(ring::AREA))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(SUGGESTION_GAP))
            .pb(px(BOTTOM))
            .child(ring)
            .children(suggestion)
    }
}

/// 中央刻度环：`ring` 是撑满整块区域的画布，中间压一个深色圆盘，`center` 放在圆盘里。
fn ring_with_disc(ring: AnyElement, center: Div) -> Div {
    let disc = ring::AREA - 2. * DISC_INSET;
    let place = || {
        div()
            .absolute()
            .top(px(DISC_INSET))
            .left(px(DISC_INSET))
            .size(px(disc))
    };
    div()
        .relative()
        .size(px(ring::AREA))
        .flex_none()
        .child(ring)
        .child(place().rounded_full().bg(theme::disc()))
        .child(
            place()
                .px(px(DISC_PADDING))
                .flex()
                .items_center()
                .justify_center()
                .child(center),
        )
}

fn signal_card(signal: &Signal) -> impl IntoElement {
    ui::card(match signal.level {
        Level::Info => theme::line(),
        Level::Warn => theme::warn_edge(),
        Level::Error => theme::error_edge(),
    })
    .flex()
    .flex_col()
    .gap(px(4.))
    .child(ui::num(
        div()
            .flex()
            .justify_between()
            .text_size(px(11.))
            .text_color(theme::muted())
            .child(signal.at.clone())
            .when(signal.repeat > 1, |s| {
                s.child(format!("×{}", signal.repeat))
            }),
    ))
    .child(SharedString::from(signal.text.clone()))
}
