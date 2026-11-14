//! 任务此刻在界面上的样子：中央圆盘的字与颜色、刻度、任务栏的小环与失败时的说明。

use gpui_kit::Hsla;
use hs_dlx_app::{Failure, Status};
use hs_m3u8_core::Stage;

use crate::ring::Ticks;
use crate::tasks::{Amount, Disk, Output, Row, finished_text};
use crate::{format, theme};

/// 一个任务此刻的样子。
pub struct Look {
    /// 圆盘里最上面的小字与它的颜色
    pub state: String,
    pub color: Hsla,
    pub ticks: Ticks,
    pub big: String,
    pub line: String,
    /// 失败时这类失败多半是怎么回事（完整的错误在信号里）；不是失败时为 None
    pub hint: Option<&'static str>,
    /// 列表小环：进度（0 到 1）、颜色与中间的简写
    pub fraction: f32,
    pub orb: Hsla,
    pub short: String,
}

/// 向下取整的百分比（`fraction` 为 0 到 1）：没做完不写 100%。
fn percent(fraction: f32) -> String {
    format!("{}%", (fraction * 100.0).floor())
}

/// 任务为什么停着。
enum Stopped<'a> {
    Paused,
    Failed(&'a Failure),
}

pub fn look(row: &Row) -> Look {
    if row.discarding {
        return discarding(row, "放弃中", "停下并删除任务目录");
    }
    match &row.task.status {
        Status::DiscardUnfinished => discarding(
            row,
            "放弃没做完",
            "任务目录可能已删了一部分，再放弃一次即可",
        ),
        Status::Running => running(row, Amount::of(row).unwrap_or_default()),
        Status::Paused => stopped(row, Stopped::Paused),
        Status::Failed(failure) => stopped(row, Stopped::Failed(failure)),
        Status::Done => done_look(row),
    }
}

/// 正在放弃或放弃没做完：刻度按已有的内容、不发光；`state` 与 `line` 为状态与说明。
fn discarding(row: &Row, state: &str, line: &str) -> Look {
    let (done, total) = Amount::of(row).map_or((0, 0), |a| (a.done, a.total));
    Look {
        state: state.to_owned(),
        color: theme::muted(),
        ticks: Ticks {
            color: theme::finished(),
            glow: false,
            ..Ticks::of(total, done, false)
        },
        big: "…".to_owned(),
        line: line.to_owned(),
        hint: None,
        fraction: 0.0,
        orb: theme::finished(),
        short: "…".to_owned(),
    }
}

/// 暂停或失败的任务：按任务目录里已完成的内容显示，刻度不发光；失败的大字与说明按种类（见 [`format::failure`]），完整的
/// 错误在信号里。点了继续、还在读任务目录时状态写「正在继续」。
fn stopped(row: &Row, why: Stopped<'_>) -> Look {
    let amount = Amount::of(row);
    let (done, total) = amount.as_ref().map_or((0, 0), |a| (a.done, a.total));
    let known = total > 0;
    let recording = row.recording();
    let state = |state: &str| if row.resuming { "正在继续" } else { state }.to_owned();
    let ticks = |lit: Hsla| Ticks {
        color: lit,
        glow: false,
        ..Ticks::of(total, done, false)
    };
    let fraction = amount.as_ref().map_or(0.0, Amount::fraction);
    match why {
        Stopped::Paused => {
            let (big, line) = kept_text(row, amount.as_ref(), recording, true);
            Look {
                state: state("已暂停"),
                color: theme::muted(),
                ticks: ticks(theme::cyan_dim()),
                short: if known && !recording {
                    big.clone()
                } else {
                    "‖".to_owned()
                },
                big,
                line,
                hint: None,
                fraction,
                orb: theme::cyan_dim(),
            }
        }
        Stopped::Failed(failure) => {
            let (big, hint) = format::failure(failure.kind);
            let (_, line) = kept_text(row, amount.as_ref(), recording, false);
            Look {
                state: state("需要处理"),
                color: theme::orange(),
                ticks: ticks(theme::finished()),
                big,
                line,
                hint: Some(hint),
                fraction,
                orb: theme::orange(),
                short: "!".to_owned(),
            }
        }
    }
}

/// 停着的任务的大字与说明：录制写已录的时长，点播写百分比与片数，`paused` 时写明继续时接着下；没有内容时说明为什么。
fn kept_text(
    row: &Row,
    amount: Option<&Amount>,
    recording: bool,
    paused: bool,
) -> (String, String) {
    let known = amount.is_some_and(|a| a.total > 0);
    let then = if paused { " · 继续时接着下" } else { "" };
    let line = match (amount, &row.disk) {
        (Some(a), _) if recording => {
            format!(
                "已录 {} · {}",
                format::clock(a.duration),
                format::bytes(a.bytes)
            )
        }
        (Some(a), _) if a.total > 0 => format!("已下 {} / {} 片{then}", a.done, a.total),
        (Some(a), _) if a.done > 0 => {
            format!("已下 {} 片 · {}{then}", a.done, format::bytes(a.bytes))
        }
        (_, Disk::Unreadable(reason)) => format!("读不出任务目录：{reason}"),
        (_, Disk::Unread) => "正在读取任务目录".to_owned(),
        // 存储里记着总片数说明下过；macOS 上外接盘卸下与目录被删分不出来
        (_, Disk::Read(None)) if row.task.total.is_some() => {
            "任务目录不在：所在的盘没接上，或被删了".to_owned()
        }
        _ => "还没有下载内容".to_owned(),
    };
    let big = match amount {
        Some(a) if recording => format::clock(a.duration),
        Some(a) if known => percent(a.fraction()),
        _ => "‖".to_owned(),
    };
    (big, line)
}

/// 已完成的任务：整圈点亮（刻度数按存储里的总片数，与下载时一致），大字为时长，下面写输出的种类、视频的宽高与大小。
fn done_look(row: &Row) -> Look {
    let total = row.task.total.unwrap_or(0);
    let ticks = Ticks::of(total, total, false);
    let result = row.task.result.as_ref();
    let big = result
        .and_then(|r| r.duration)
        .map_or_else(|| "✓".to_owned(), format::clock);
    Look {
        state: "已完成".to_owned(),
        color: theme::muted(),
        ticks: Ticks {
            lit: ticks.count,
            glow: true,
            ..ticks
        },
        big,
        line: result.map_or_else(
            || row.file_name.clone(),
            |r| finished_text(&row.task.spec.output, r),
        ),
        hint: None,
        fraction: 1.0,
        orb: theme::finished(),
        short: "✓".to_owned(),
    }
}

fn running(row: &Row, amount: Amount) -> Look {
    let stage = row.progress.as_ref().map_or(Stage::Preparing, |p| p.stage);
    if stage == Stage::Writing {
        let writing = row.writing();
        let kind = match writing {
            Some((Output::Mp4, _)) => "写出 MP4",
            Some((Output::Hls, _)) => "写出本地 HLS",
            None => "写出",
        };
        let count = writing.map(|(_, count)| count);
        let written = row.written_share().unwrap_or(0.0);
        let percent = percent(written);
        let ticks = Ticks::of(amount.total, amount.done, false);
        return Look {
            state: kind.to_owned(),
            color: theme::cyan(),
            ticks: Ticks {
                lit: ticks.count,
                written: (written * ticks.count as f32) as usize,
                ..ticks
            },
            big: percent.clone(),
            line: count.map_or_else(
                || "不联网".to_owned(),
                |c| format!("已写 {} / {} 个文件 · 不联网", c.done, c.total),
            ),
            hint: None,
            fraction: written,
            orb: theme::cyan_pale(),
            short: percent,
        };
    }
    if row.task.write_only {
        return Look {
            state: "准备写出".to_owned(),
            color: theme::cyan(),
            ticks: Ticks::of(0, 0, true),
            big: format::clock(amount.duration.max(row.recorded_before_writing)),
            line: "不联网，只写出已录的".to_owned(),
            hint: None,
            fraction: 0.0,
            orb: theme::cyan(),
            short: "…".to_owned(),
        };
    }
    if row.recording() {
        return Look {
            state: "● 录制中".to_owned(),
            color: theme::red(),
            ticks: Ticks {
                color: theme::red(),
                ..Ticks::of(amount.total, amount.done, true)
            },
            big: format::clock(amount.duration),
            line: format!("{} 片 · {}", amount.done, format::bytes(amount.bytes)),
            hint: None,
            fraction: 1.0,
            orb: theme::red(),
            short: "REC".to_owned(),
        };
    }
    if amount.total == 0 {
        return Look {
            state: "准备中".to_owned(),
            color: theme::cyan(),
            ticks: Ticks::of(0, 0, true),
            big: "—".to_owned(),
            line: "拉取播放列表".to_owned(),
            hint: None,
            fraction: 0.0,
            orb: theme::cyan(),
            short: "…".to_owned(),
        };
    }
    let mut ticks = Ticks::of(amount.total, amount.done, true);
    ticks.retrying = row
        .progress
        .as_ref()
        .is_some_and(|p| !p.retrying.is_empty());
    Look {
        state: "下载中".to_owned(),
        color: theme::cyan(),
        ticks,
        big: percent(amount.fraction()),
        line: format!("{} / {} 片", amount.done, amount.total),
        hint: None,
        fraction: amount.fraction(),
        orb: theme::cyan(),
        short: percent(amount.fraction()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hs_dlx_app::{Failure, FailureKind, Finished, Status};
    use hs_m3u8_core::{FileCount, Progress, SegmentCounts, Stage, Writing};

    use super::look;
    use crate::tasks::Disk;
    use crate::tasks::sample::{row, saved};

    /// 暂停的任务按任务目录里读到的与存储里的总片数显示；读之前、读不出、目录不在各有说法。
    #[test]
    fn paused_tasks_show_what_is_in_the_work_dir() {
        let mut paused = row(Status::Paused, false, Some(160));
        assert_eq!(look(&paused).line, "正在读取任务目录");
        paused.disk = Disk::Unreadable("权限不够".into());
        assert_eq!(look(&paused).line, "读不出任务目录：权限不够");
        paused.disk = Disk::Read(None);
        assert!(look(&paused).line.starts_with("任务目录不在"));
        paused.disk = Disk::Read(Some(saved(40, false)));
        let seen = look(&paused);
        assert_eq!((seen.big.as_str(), seen.fraction), ("25%", 0.25));
        assert_eq!((seen.ticks.count, seen.ticks.lit), (160, 40));
        paused.task.total = None;
        assert_eq!(look(&paused).line, "已下 40 片 · 4.0 MB · 继续时接着下");
    }

    /// 继续后在准备阶段就失败：这次运行的进度是 0/0，读到任务目录后按目录里已有的显示。
    #[test]
    fn a_resume_that_fails_early_still_shows_the_work_dir() {
        let forbidden = Failure {
            kind: FailureKind::Status(403),
            message: "HTTP 状态 403".into(),
        };
        let mut failed = row(Status::Failed(forbidden), false, Some(420));
        failed.progress = Some(Progress::default());
        failed.disk = Disk::Read(Some(saved(143, false)));
        let seen = look(&failed);
        assert_eq!((seen.ticks.count, seen.ticks.lit), (360, 122));
        assert_eq!(seen.big, "403");
        assert!(seen.hint.is_some_and(|hint| hint.contains("Referer")));
        assert_eq!(seen.line, "已下 143 / 420 片");
    }

    /// 暂停的直播录制写已录的时长，不写片数与百分比。
    #[test]
    fn paused_recordings_show_recorded_time() {
        let mut paused = row(Status::Paused, true, None);
        paused.disk = Disk::Read(Some(saved(40, true)));
        let seen = look(&paused);
        assert_eq!((seen.big.as_str(), seen.short.as_str()), ("00:40", "‖"));
        assert!(seen.line.starts_with("已录 00:40"), "{}", seen.line);
    }

    /// 写出时整圈都已完成，按写出的文件比例点亮白色；百分比向下取整。
    #[test]
    fn writing_lights_the_written_share() {
        let mut writing = row(Status::Running, false, None);
        writing.progress = Some(Progress {
            stage: Stage::Writing,
            segments: SegmentCounts {
                done: 160,
                total: 160,
                ..SegmentCounts::default()
            },
            writing: Some(Writing {
                mp4: Some(FileCount {
                    done: 159,
                    total: 160,
                }),
                hls: None,
            }),
            ..Progress::default()
        });
        let seen = look(&writing);
        assert_eq!(
            (seen.state.as_str(), seen.big.as_str()),
            ("写出 MP4", "99%")
        );
        assert_eq!((seen.ticks.lit, seen.ticks.written), (160, 159));
    }

    #[test]
    fn finished_tasks_describe_their_output() {
        let mut done = row(Status::Done, false, Some(10));
        done.task.result = Some(Finished {
            duration: Some(Duration::from_secs(48 * 60 + 12)),
            bytes: 1_420_000_000,
            resolution: Some((1920, 1080)),
        });
        let seen = look(&done);
        assert_eq!((seen.ticks.count, seen.ticks.lit), (60, 60));
        assert_eq!(seen.big, "48:12");
        assert_eq!(seen.line, "MP4 · 1920×1080 · 1.4 GB");
    }
}
