//! 选中任务的舞台与检查面板：舞台中间是表盘（直播录制是钟面），检查面板是读数、信号与操作；没有任务时的待机页。

use std::f32::consts::TAU;
use std::time::Instant;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use hs_m3u8_app::{OutputSpec, Status};
use hs_m3u8_core::{Proxy, bare_url};

use super::look::{Look, look};
use super::plan::{Action, Plan, stopped_plan};
use super::{Shell, live_view};
use crate::links::{self, http_url};
use crate::signals::{Level, Signal};
use crate::tasks::{Amount, Row, Tasks, resume_label};
use crate::ui::{self, GAP, INSPECTOR_MIN_WIDTH, TEXT_LINE, Tone};
use crate::{dial, format, ring, theme};

/// 待机时彗尾扫一圈的秒数，与背光起伏一次的秒数。
const STANDBY_TURN: f32 = 20.;
const BREATH: f32 = 6.;
/// 「能粘贴的」小图形的边长。
const KIND_ICON: f32 = 28.;
/// 信号的时刻一栏的宽：放得下「11-14 10:20」。
const SIGNAL_AT_WIDTH: f32 = 72.;
/// 任务页的最小宽：表盘与收到最窄的检查面板。
pub(super) const MIN_WIDTH: f32 = ring::AREA + GAP + INSPECTOR_MIN_WIDTH;
/// 任务页的最小高：放得下表盘与粘贴栏的舞台。
pub(super) const MIN_HEIGHT: f32 = ui::stage_height(ring::AREA);

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
                readout(&look),
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
                        .text_color(theme::cyan())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::cyan_pale()))
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
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .p(px(14.))
                .rounded(px(14.))
                .bg(theme::cell())
                .border_1()
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

    /// 暂停或失败的任务的操作（见 [`stopped_plan`]）：第一个是主操作（失败的为橙色），其余在下面一行。
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
                    Some(format!("不联网，写出 {}", format::clock(row.recorded())).into()),
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
            let folder = folder_of(&row.task.spec.output);
            let on_click = cx.listener(move |this, _, window, cx| match act {
                Action::EditHeaders => this.edit_headers(id, window, cx),
                Action::WriteRecorded => this.write_recorded(id, cx),
                Action::SwitchToHls => this.switch_to_hls(id, cx),
                Action::Resume => this.resume(id, cx),
                Action::OpenFolder => cx.open_with_system(&folder),
            });
            if index == 0 {
                let tone = if failed { Tone::Amber } else { Tone::Glow };
                primary = Some(ui::primary(key, label, detail, tone, true, on_click));
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
                    .border_color(theme::orange())
                    .text_color(theme::orange())
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
                    Some(format!("写出已录的 {}", format::clock(row.recorded())).into()),
                    Tone::Rec,
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
                        Tone::Glow,
                        true,
                        move |_, _, cx| cx.open_with_system(&mp4),
                    )
                });
                let mut rest = Vec::new();
                if let Some(shown) = shown {
                    rest.push(
                        ui::button(
                            "reveal",
                            "在访达中显示",
                            Tone::Plain,
                            move |_, _, cx| cx.reveal_path(&shown),
                        )
                        .flex_1(),
                    );
                }
                rest.push(forget(Tone::Ghost, cx));
                Foot { primary, rest }
            }
        }
    }

    /// 没有任务时的待机页：舞台是垫着刻度面的表盘，彗尾慢慢扫、背光起伏（逐帧重画见 [`Shell::sync_standby`]）；输入时
    /// 背光提亮，框里是可探测的链接时整圈蓄满、圆盘写认出了什么。检查面板是新任务照着开始的设置与剪贴板里的链接。
    pub(super) fn render_standby(
        &self,
        tasks: &Tasks,
        window: &Window,
        cx: &Context<Self>,
    ) -> (Div, Div) {
        let dock = self.dock.read(cx);
        let typed = dock.text(cx);
        let link = http_url(&typed);
        let animate = !cx.reduce_motion();
        let elapsed = self.standby_since.elapsed().as_secs_f32();
        let breath = if animate {
            (elapsed * TAU / BREATH).sin() * 0.025
        } else {
            0.
        };
        let lift = match (&link, dock.typing(window, cx)) {
            (Some(_), _) => 0.07,
            (None, true) => 0.03,
            (None, false) => 0.,
        };
        let head = if animate { elapsed / STANDBY_TURN } else { 0. };
        let charged = link.is_some();
        let figure = dial::figure(move |bounds, window, _| {
            if charged {
                let ticks = ring::Ticks {
                    count: 120,
                    lit: 120,
                    ..ring::Ticks::of(0, 0, false)
                };
                ring::paint_ticks(bounds, ring::RADIUS, &ticks, window)
            } else {
                ring::paint_comet(bounds, ring::RADIUS, head, theme::cyan(), window)
            }
        });
        let readout = match &link {
            Some(url) => {
                let stream = links::stream_url(url.as_str()).is_some();
                standby_readout(
                    if stream {
                        "认出 m3u8 地址"
                    } else {
                        "认出链接"
                    },
                    url.host_str().unwrap_or_default().to_owned(),
                    if stream {
                        "回车探测清晰度与音轨"
                    } else {
                        "回车探测；是网页就在后台找视频流"
                    },
                )
            }
            None => standby_readout(
                "就绪",
                "粘贴链接开始".to_owned(),
                "m3u8 地址，或有视频的网页",
            ),
        };
        let figure = dial::lit_dial(
            theme::cyan(),
            dial::LIGHT_STRENGTH + breath + lift,
            true,
            figure,
            readout,
        );
        let stage = ui::stage(ring::AREA, figure, None, self.dock.clone());
        (stage, self.standby_inspector(tasks, cx))
    }

    /// 待机页的检查面板：新任务照着开始的保存位置、可用空间与网络，剪贴板里的链接，与能粘贴哪两种链接。
    fn standby_inspector(&self, tasks: &Tasks, cx: &Context<Self>) -> Div {
        let free = match tasks.free_space() {
            None => "—".to_owned(),
            Some(Ok(bytes)) => format::bytes(*bytes),
            Some(Err(_)) => "读不出".to_owned(),
        };
        let network = match tasks.service().settings() {
            Ok(settings) => match settings.proxy {
                Proxy::System => "跟随系统",
                Proxy::Direct => "直连",
                Proxy::Url(_) => "指定代理",
            },
            Err(_) => "读不出设置",
        };
        let mut body = vec![
            ui::reads(
                vec![
                    ("可用空间".into(), free.into()),
                    ("网络".into(), network.into()),
                ],
                Some((
                    "保存到".into(),
                    format::path(tasks.folder(), self.home.as_deref()).into(),
                )),
            )
            .into_any_element(),
        ];
        if let Some(url) = self.clipboard.clone() {
            let host = url.host_str().unwrap_or_default().to_owned();
            let path = url.path().to_owned();
            body.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(ui::caption("剪贴板里的链接"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .py(px(12.))
                            .pl(px(14.))
                            .pr(px(12.))
                            .rounded(px(14.))
                            .bg(theme::cell())
                            .border_1()
                            .border_color(theme::deck_edge())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .text_size(px(13.5))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .truncate()
                                            .child(host),
                                    )
                                    .child(ui::num(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(theme::muted())
                                            .truncate()
                                            .child(path),
                                    )),
                            )
                            .child(ui::button(
                                "probe-clipboard",
                                "探测",
                                Tone::Plain,
                                cx.listener(move |this, _, window, cx| {
                                    this.probe_text(url.as_str(), window, cx)
                                }),
                            )),
                    )
                    .into_any_element(),
            );
        }
        body.push(
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(ui::caption("能粘贴的"))
                .child(kind(
                    ui::rings(KIND_ICON, 2.5, 14.).into_any_element(),
                    "m3u8 地址",
                    "直接探测出几档清晰度与音轨",
                ))
                .child(kind(
                    page_icon(),
                    "有视频的网页",
                    "在后台打开，找出里面的视频流",
                ))
                .into_any_element(),
        );
        ui::inspector("准备就绪", "新任务照这些开始", body, [])
    }
}

/// 待机表盘圆盘里的字：状态、大字（主机名或「粘贴链接开始」）与一行说明。
fn standby_readout(state: &'static str, big: String, line: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(10.))
        .text_center()
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme::cyan())
                .child(state),
        )
        .child(ui::num(
            div()
                .max_w(px(dial::TEXT_WIDTH))
                .text_size(px(26.))
                .font_weight(FontWeight::BOLD)
                .truncate()
                .child(big),
        ))
        .child(
            div()
                .max_w(px(dial::TEXT_WIDTH))
                .text_size(px(13.))
                .text_color(theme::muted())
                .child(line),
        )
}

/// 「能粘贴的」一行：图形、名字与它会怎样。
fn kind(icon: AnyElement, name: &'static str, does: &'static str) -> Div {
    div().flex().items_start().gap(px(12.)).child(icon).child(
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .text_size(px(13.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(name),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(theme::muted())
                    .child(does),
            ),
    )
}

/// 网页的小图形：圆角方框里两道横线。
fn page_icon() -> AnyElement {
    let bar = |width: f32| div().h(px(2.)).w(px(width)).bg(theme::hover_edge());
    div()
        .size(px(KIND_ICON))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(3.))
        .p(px(6.))
        .rounded(px(7.))
        .border_2()
        .border_color(theme::hover_edge())
        .child(bar(12.))
        .child(bar(8.))
        .into_any_element()
}

/// 圆盘里的字：状态（带颜色）、大字与下面一行说明。
fn readout(look: &Look) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(13.))
                .text_color(look.color)
                .child(look.state.clone()),
        )
        .child(ui::num(
            div()
                .text_size(px(66.))
                .line_height(px(66.))
                .font_weight(FontWeight::BOLD)
                .child(look.big.clone()),
        ))
        .child(ui::num(
            div()
                .max_w(px(dial::TEXT_WIDTH))
                .text_size(px(13.))
                .line_height(px(TEXT_LINE))
                .text_color(theme::muted())
                .text_center()
                .line_clamp(2)
                .child(SharedString::from(look.line.clone())),
        ))
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
        cells.push(cell(
            "保存为",
            output_kind(&row.task.spec.output).to_owned(),
        ));
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
                .map_or_else(
                    || "—".to_owned(),
                    |left| format::remaining(left).replacen("剩余 ", "", 1),
                );
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

/// 输出的种类。
fn output_kind(output: &OutputSpec) -> &'static str {
    match output {
        OutputSpec::Mp4(_) => "MP4",
        OutputSpec::Hls(_) => "本地 HLS",
        OutputSpec::Both { .. } => "MP4 与本地 HLS",
    }
}

/// 输出所在的目录：占着输出路径的东西就在里面。
fn folder_of(output: &OutputSpec) -> std::path::PathBuf {
    let target = output.options().target;
    target
        .mp4()
        .or(target.hls())
        .and_then(|path| path.parent())
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
                Level::Warn => theme::warn_text(),
                Level::Error => theme::error_text(),
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
