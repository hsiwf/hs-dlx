//! 选中任务的舞台与检查面板：舞台中间是表盘（直播录制是钟面），检查面板是读数、信号与操作。

use std::time::Instant;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, div, px,
};
use hs_dlx_app::Status;
use hs_m3u8_core::bare_url;

use super::look::{Look, look};
use super::plan::{Action, Plan, stopped_plan};
use super::{Shell, live_view};
use crate::signals::{Level, Signal};
use crate::tasks::{Amount, Row, main_output, resume_label, saved_as};
use crate::ui::{self, GAP, INSPECTOR_MIN_WIDTH, Solid, Tone};
use crate::{dial, format, ring, theme};

/// 在系统的文件管理器里显示输出：各平台的叫法。
const REVEAL: &str = if cfg!(target_os = "macos") {
    "在访达中显示"
} else if cfg!(target_os = "windows") {
    "在资源管理器中显示"
} else {
    "在文件夹中显示"
};
/// 信号的时刻一栏的宽：放得下「11-14 10:20」。
const SIGNAL_AT_WIDTH: f32 = 72.;
/// 任务页的最小宽：表盘与收到最窄的检查面板。
pub(super) const MIN_WIDTH: f32 = ring::AREA + GAP + INSPECTOR_MIN_WIDTH;
/// 任务页的最小高：放得下表盘与粘贴栏的舞台。
pub(super) const MIN_HEIGHT: f32 = ui::stage_height(ring::AREA, 0.);

/// 检查面板底栏里的操作：最该按的一个与其余的。
struct Foot {
    primary: Option<Stateful<Div>>,
    rest: Vec<Stateful<Div>>,
}

impl Shell {
    /// 舞台与检查面板。
    pub(super) fn render_task(&self, row: &Row, now: Instant, cx: &Context<Self>) -> (Div, Div) {
        let look = look(row);
        // 直播录制（没完成的）画钟面
        let live = row.recording() && row.task.status != Status::Done;
        let figure = if live {
            live_view::figure(row, &look)
        } else {
            let ticks = look.ticks;
            dial::dial(
                look.color,
                dial::figure(move |bounds, window, _| {
                    ring::paint_ticks(bounds, ring::RADIUS, &ticks, window)
                }),
                dial::readout(
                    look.state.clone(),
                    look.color,
                    dial::Big::Figure(look.big.clone()),
                    look.line.clone(),
                ),
            )
        };
        let stage = ui::stage(
            ring::AREA,
            figure,
            self.clipboard_hint(cx),
            self.dock.clone(),
        );
        (stage, self.inspector(row, &look, live, now, cx))
    }

    /// 剪贴板里有链接、粘贴栏空着时，粘贴栏上面的一行。
    fn clipboard_hint(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let url = self.clipboard.clone()?;
        if !self.dock.read(cx).text(cx).is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .max_w_full()
                .text_size(px(12.))
                .text_color(theme::muted())
                .child(ui::num(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(format!("剪贴板：{}", bare_url(&url))),
                ))
                .child(
                    div()
                        .id("probe-clipboard-hint")
                        .flex_none()
                        .text_color(theme::accent())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::accent_pale()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.probe_text(url.as_str(), window, cx)
                        }))
                        .child("探测"),
                )
                .into_any_element(),
        )
    }

    fn inspector(
        &self,
        row: &Row,
        look: &Look,
        live: bool,
        now: Instant,
        cx: &Context<Self>,
    ) -> Div {
        let host = row.task.spec.url.host_str().unwrap_or_default().to_owned();
        let mut body: Vec<AnyElement> = Vec::new();
        if let Some(card) = self.headers_card(row, cx) {
            body.push(card.into_any_element());
        }
        let cells = if live {
            live_view::reads(row)
        } else {
            reads(row, now)
        };
        if !cells.is_empty() {
            body.push(ui::reads(cells, None).into_any_element());
        }
        if live {
            body.push(live_view::legend(row).into_any_element());
        }
        body.push(signals(&row.signals).into_any_element());
        let Foot { primary, rest } = self.foot(row, cx);
        let mut foot: Vec<AnyElement> = Vec::new();
        if let Some(hint) = look.hint {
            foot.push(ui::note(hint, true).into_any_element());
        }
        foot.extend(primary.map(IntoElement::into_any_element));
        if !rest.is_empty() {
            foot.push(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(10.))
                    .children(rest)
                    .into_any_element(),
            );
        }
        ui::inspector(row.task.spec.title.clone(), host, body, foot)
    }

    /// 正在改这个任务的请求头时，检查面板最上面的编辑卡片。
    fn headers_card(&self, row: &Row, cx: &Context<Self>) -> Option<Div> {
        let (_, editor) = self
            .editing_headers
            .as_ref()
            .filter(|(id, _)| *id == row.task.id)?;
        let resume = self.plan(row).resume_after_headers;
        let failed = matches!(row.task.status, Status::Failed(_));
        let label = if resume {
            format!("保存并{}", resume_label(failed, row.task.write_only))
        } else {
            "保存".to_owned()
        };
        Some(
            ui::well()
                .flex()
                .flex_col()
                .gap(px(12.))
                .p(px(14.))
                // 正在编辑：描边比别的卡片亮一档
                .border_color(theme::edge())
                .child(ui::caption("请求头"))
                .child(editor.clone())
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(
                            ui::button(
                                "save-headers",
                                label,
                                Tone::Glow,
                                cx.listener(move |this, _, window, cx| {
                                    this.save_headers(resume, window, cx)
                                }),
                            )
                            .flex_1(),
                        )
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

    /// 暂停或失败的任务的操作（见 [`stopped_plan`]）：第一个是主操作（失败的为警告色），其余在下面一行。
    fn stopped_foot(&self, row: &Row, discard: Stateful<Div>, cx: &Context<Self>) -> Foot {
        let id = row.task.id;
        let failed = matches!(row.task.status, Status::Failed(_));
        let plan = self.plan(row);
        let mut primary = None;
        let mut rest = Vec::new();
        for (index, action) in plan.actions.iter().enumerate() {
            let (key, label, detail): (&'static str, String, Option<SharedString>) = match action {
                Action::EditHeaders => (
                    "edit-headers",
                    "改请求头".to_owned(),
                    plan.resume_after_headers.then(|| "保存后接着重试".into()),
                ),
                Action::WriteRecorded => (
                    "write-recorded",
                    "写出已录的".to_owned(),
                    // 不知道已录多久（任务目录读不出、还没报进度）时不写，不写成 00:00
                    Amount::of(row).map(|amount| {
                        format!("不联网，写出 {}", format::clock(amount.duration)).into()
                    }),
                ),
                Action::SwitchToHls => (
                    "switch-hls",
                    "改存本地 HLS".to_owned(),
                    Some("已下的照样用".into()),
                ),
                Action::Resume => (
                    "resume",
                    resume_label(failed, row.task.write_only).to_owned(),
                    None,
                ),
                Action::OpenFolder => ("open-folder", "打开保存目录".to_owned(), None),
            };
            let act = *action;
            let folder = folder_of(&row.task.spec);
            let on_click = cx.listener(move |this, _, window, cx| match act {
                Action::EditHeaders => this.edit_headers(id, window, cx),
                Action::WriteRecorded => this.write_recorded(id, cx),
                Action::SwitchToHls => this.switch_to_hls(id, cx),
                Action::Resume => this.resume(id, cx),
                Action::OpenFolder => cx.open_with_system(&folder),
            });
            if index == 0 {
                let solid = if failed { Solid::Amber } else { Solid::Glow };
                primary = Some(ui::primary(key, label, detail, solid, true, on_click));
            } else {
                rest.push(ui::button(key, label, Tone::Plain, on_click).flex_1());
            }
        }
        rest.push(discard);
        Foot { primary, rest }
    }

    fn foot(&self, row: &Row, cx: &Context<Self>) -> Foot {
        if !row.idle() {
            return Foot {
                primary: None,
                rest: Vec::new(),
            };
        }
        let id = row.task.id;
        let confirming = self.confirm_discard == Some(id);
        let discard = |cx: &Context<Self>| {
            let button = ui::button(
                "discard",
                if confirming {
                    "再点一次：删除已下的内容"
                } else {
                    "放弃任务"
                },
                if confirming { Tone::Plain } else { Tone::Ghost },
                cx.listener(move |this, _, _, cx| this.discard(id, confirming, cx)),
            );
            // 确认时独占一行，写清后果
            if confirming {
                button
                    .w_full()
                    .border_color(theme::warn())
                    .text_color(theme::warn())
            } else {
                button
            }
        };
        let pause = |cx: &Context<Self>| {
            ui::button(
                "pause",
                "暂停",
                Tone::Plain,
                cx.listener(move |this, _, _, cx| this.tasks.read(cx).pause(id)),
            )
            .flex_1()
        };
        let forget = |tone: Tone, cx: &Context<Self>| {
            ui::button(
                "forget",
                "从列表移除",
                tone,
                cx.listener(move |this, _, _, cx| {
                    this.tasks.update(cx, |tasks, cx| tasks.forget(id, cx))
                }),
            )
        };
        match &row.task.status {
            Status::DiscardUnfinished => Foot {
                primary: None,
                rest: vec![discard(cx).flex_1()],
            },
            Status::Running if row.stoppable() => Foot {
                primary: Some(ui::primary(
                    "stop",
                    "停止并写出",
                    Amount::of(row).map(|amount| {
                        format!("写出已录的 {}", format::clock(amount.duration)).into()
                    }),
                    Solid::Rec,
                    true,
                    cx.listener(move |this, _, _, cx| {
                        this.tasks.read(cx).stop(id);
                        cx.notify();
                    }),
                )),
                rest: vec![pause(cx), discard(cx)],
            },
            Status::Running => Foot {
                primary: None,
                rest: vec![pause(cx), discard(cx)],
            },
            // 没有自己的任务目录（与别的任务共用、迁移时让了出去）：不能继续，放弃会删别人的，只能移除记录
            Status::Paused | Status::Failed(_) if row.task.work_dir.is_none() => Foot {
                primary: None,
                rest: vec![forget(Tone::Plain, cx).flex_1()],
            },
            Status::Paused | Status::Failed(_) => self.stopped_foot(row, discard(cx), cx),
            Status::Done => {
                // 输出在不在是完成时与打开应用时在后台查的；还没查完时先不给这两个按钮
                let (mp4, shown) = row
                    .present
                    .as_ref()
                    .map_or((None, None), |p| (p.mp4.clone(), p.shown.clone()));
                let primary = mp4.map(|mp4| {
                    ui::primary(
                        "play",
                        "播放",
                        Some(row.file_name.clone().into()),
                        Solid::Glow,
                        true,
                        move |_, _, cx| cx.open_with_system(&mp4),
                    )
                });
                let mut rest = Vec::new();
                if let Some(shown) = shown {
                    rest.push(
                        ui::button("reveal", REVEAL, Tone::Plain, move |_, _, cx| {
                            cx.reveal_path(&shown)
                        })
                        .flex_1(),
                    );
                }
                rest.push(forget(Tone::Ghost, cx));
                Foot { primary, rest }
            }
        }
    }
}

/// 检查面板的读数（直播录制的见 [`live_view::reads`]）：下载中为速度、剩余、已下与分片，写出中为已下与分片，停着的为
/// 已下的片数与大小，完成的为时长、大小、画面与输出；还没有内容时为空。
fn reads(row: &Row, now: Instant) -> Vec<(SharedString, SharedString)> {
    let cell = |label: &'static str, value: String| (SharedString::from(label), value.into());
    if row.task.status == Status::Done {
        let Some(result) = row.task.result.as_ref() else {
            return Vec::new();
        };
        let mut cells = vec![
            cell(
                "时长",
                result
                    .duration
                    .map_or_else(|| "—".to_owned(), format::clock),
            ),
            cell("大小", format::bytes(result.bytes)),
        ];
        if let Some((width, height)) = result.resolution {
            cells.push(cell("画面", format!("{width}×{height}")));
        }
        cells.push(cell("保存为", saved_as(&row.task.spec).to_owned()));
        return cells;
    }
    let Some(amount) = Amount::of(row) else {
        return Vec::new();
    };
    let Amount {
        done, total, bytes, ..
    } = amount;
    let segments = if total > 0 {
        format!("{done} / {total}")
    } else {
        done.to_string()
    };
    match row.task.status {
        Status::Running if row.written_share().is_some() => {
            vec![cell("已下", format::bytes(bytes)), cell("分片", segments)]
        }
        Status::Running if total > 0 && !row.task.write_only => {
            let speed = format!("{}/s", format::bytes(row.rate.bytes_per_second(now) as u64));
            let left = row
                .rate
                .remaining(now, total.saturating_sub(done))
                .map_or_else(|| "—".to_owned(), format::duration);
            vec![
                cell("速度", speed),
                cell("剩余", left),
                cell("已下", format::bytes(bytes)),
                cell("分片", segments),
            ]
        }
        Status::Running => Vec::new(),
        _ if done == 0 => Vec::new(),
        _ => vec![
            cell("已下", format!("{segments} 片")),
            cell("大小", format::bytes(bytes)),
        ],
    }
}

/// 输出所在的目录：占着输出路径的东西就在里面。
fn folder_of(spec: &hs_dlx_app::TaskSpec) -> std::path::PathBuf {
    main_output(spec)
        .parent()
        .expect("输出是以文件名结尾的绝对路径")
        .to_path_buf()
}

/// 信号：最新的在最上面，左边是时刻，字色是级别。
fn signals(signals: &[Signal]) -> Div {
    div()
        .flex()
        .flex_col()
        .child(div().pb(px(6.)).child(ui::caption("信号")))
        .children(signals.iter().rev().map(|signal| {
            let color = match signal.level {
                Level::Info => theme::soft(),
                Level::Warn => theme::signal_warn(),
                Level::Error => theme::signal_error(),
            };
            div()
                .flex()
                .gap(px(10.))
                .py(px(7.))
                .border_b_1()
                .border_color(theme::hairline())
                .text_size(px(13.))
                .line_height(px(19.))
                .child(ui::num(
                    div()
                        .w(px(SIGNAL_AT_WIDTH))
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(theme::faint())
                        .child(signal.at.clone()),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(color)
                        .child(SharedString::from(signal.text.clone()))
                        .when(signal.repeat > 1, |s| {
                            s.child(ui::num(
                                div()
                                    .text_color(theme::faint())
                                    .child(format!("×{}", signal.repeat)),
                            ))
                        }),
                )
        }))
}
