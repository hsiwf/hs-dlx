//! 任务列表里的一行：存储里的任务，加上这次运行收到的进度、结果、信号与速度；以及由它得出的样子。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{Local, TimeZone as _};
use gpui_kit::Hsla;
use hs_m3u8_app::{Finished, OutputSpec, Status, Task};
use hs_m3u8_core::{Progress, Saved, Stage};

use crate::live::Timeline;
use crate::rate::Rate;
use crate::ring::Ticks;
use crate::signals::{self, Level, Signal};
use crate::{format, theme};

/// 任务目录里已完成的内容读到了哪一步。
pub enum Disk {
    Unread,
    /// 读不出：原因
    Unreadable(String),
    /// 读到了；目录或其中的记录不在时为 None
    Read(Option<Saved>),
}

/// 完成任务的输出此刻在不在：完成时与打开应用时在后台查一次，渲染时不碰文件系统。
pub struct Present {
    /// 还在的 MP4，可以播放
    pub mp4: Option<PathBuf>,
    /// 在访达里显示哪个：MP4，没有时为本地 HLS 目录
    pub shown: Option<PathBuf>,
}

pub struct Row {
    pub task: Task,
    /// 这次运行最近的进度；继续时清掉
    pub progress: Option<Progress>,
    /// 任务不在运行时任务目录里已完成的内容；停下时作废重读，运行中留着继续前读到的
    pub disk: Disk,
    /// 读任务目录的代次：每次作废 `disk` 时加一，只收发起时代次相同的结果
    pub reading: u64,
    /// 完成任务的输出在不在；还没查时为 None
    pub present: Option<Present>,
    pub rate: Rate,
    /// 按时间先后，最多 [`signals::KEEP`] 条
    pub signals: Vec<Signal>,
    /// 输出的文件名（同时要 MP4 与本地 HLS 时为 MP4 的）
    pub file_name: String,
    /// 点了继续、还在读任务目录：操作都不可用
    pub resuming: bool,
    /// 正在放弃：操作都不可用
    pub discarding: bool,
    /// 直播录制这次打开应用以来的时间线（钟面用）
    pub timeline: Timeline,
}

impl Row {
    pub fn new(task: Task, now: Instant) -> Row {
        // 存储里的时间是本应用写的 Unix 秒；换不成本地时刻（超出 chrono 的范围）时不写时刻
        let at = Local
            .timestamp_opt(task.created_at, 0)
            .earliest()
            .map_or_else(|| "—".to_owned(), |t| t.format("%m-%d %H:%M").to_string());
        let target = task.spec.output.options().target;
        let file_name = target
            .mp4()
            .or(target.hls())
            .expect("输出至少有 MP4 与本地 HLS 之一")
            .file_name()
            .expect("存储读回任务时已校验输出路径以文件名结尾")
            .to_string_lossy()
            .into_owned();
        let mut signals = Vec::new();
        signals::push(
            &mut signals,
            at,
            Level::Info,
            format!("新建 · 保存为 {file_name}"),
        );
        Row {
            task,
            progress: None,
            disk: Disk::Unread,
            reading: 0,
            present: None,
            rate: Rate::new(now, Some(0)),
            signals,
            file_name,
            resuming: false,
            discarding: false,
            timeline: Timeline::default(),
        }
    }

    pub fn signal(&mut self, level: Level, text: String) {
        let at = Local::now().format("%H:%M:%S").to_string();
        signals::push(&mut self.signals, at, level, text);
    }

    /// 能不能操作：正在继续或放弃时不能。
    pub fn idle(&self) -> bool {
        !self.resuming && !self.discarding
    }

    /// 已录（直播）或已下（点播）的第 0 条轨时长：运行中按进度，停下的按任务目录里读到的；都没有时为 0。
    pub fn recorded(&self) -> Duration {
        Amount::of(self).map_or(Duration::ZERO, |a| a.duration)
    }

    /// 已完成的片数与字节数，来源同 [`Row::recorded`]；都没有时为 None。
    pub fn completed(&self) -> Option<(usize, u64)> {
        Amount::of(self).map(|a| (a.done, a.bytes))
    }

    /// 作废读到的任务目录内容（任务停下时：内容变了），之后发起的读才算数。
    pub fn forget_disk(&mut self) {
        self.disk = Disk::Unread;
        self.reading += 1;
    }

    /// 是直播的录制：运行中过了准备阶段的按阶段；其余（停下的、还在准备的）按任务目录里的记录（继续前读到的），
    /// 没读到时按新建时的选择。按直播新建、实际是点播的，进度到了下载阶段或读到任务目录后就不算。
    pub fn recording(&self) -> bool {
        if let Some(p) = &self.progress
            && self.task.status == Status::Running
        {
            match p.stage {
                Stage::Recording => return true,
                Stage::Downloading | Stage::Writing | Stage::Done => return false,
                Stage::Preparing => {}
            }
        }
        match &self.disk {
            Disk::Read(Some(saved)) => saved.live,
            _ => self.task.spec.live,
        }
    }
}

/// 一个任务此刻的样子。
pub struct Look {
    /// 圆盘里最上面的小字与它的颜色
    pub state: String,
    pub color: Hsla,
    pub ticks: Ticks,
    pub big: String,
    pub line: String,
    /// 列表小环：进度（0 到 1）、颜色与中间的简写
    pub fraction: f32,
    pub orb: Hsla,
    pub short: String,
}

/// 已完成、合计（不知道时为 0）、字节数与第 0 条轨的时长。
#[derive(Default)]
struct Amount {
    done: usize,
    total: usize,
    bytes: u64,
    duration: Duration,
}

impl Amount {
    /// 运行中取这次运行的进度；停下的取任务目录里读到的（与存储里的总片数），还没读到时取这次运行留下的进度。
    fn of(row: &Row) -> Option<Amount> {
        let from_progress = |p: &Progress| Amount {
            done: p.segments.done,
            total: p.segments.total,
            bytes: p.bytes,
            duration: p.duration,
        };
        if row.task.status == Status::Running {
            return row.progress.as_ref().map(from_progress);
        }
        match &row.disk {
            Disk::Read(Some(saved)) => Some(Amount {
                done: saved.segments,
                total: row.task.total.unwrap_or(0),
                bytes: saved.bytes,
                duration: saved.duration,
            }),
            Disk::Read(None) => None,
            Disk::Unread | Disk::Unreadable(_) => row.progress.as_ref().map(from_progress),
        }
    }

    fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f32 / self.total as f32).min(1.0)
        }
    }
}

/// 向下取整的百分比（`fraction` 为 0 到 1）：没做完不写 100%。
fn percent(fraction: f32) -> String {
    format!("{}%", (fraction * 100.0).floor())
}

/// 任务为什么停着。
enum Stopped<'a> {
    Paused,
    /// 值为错误的说明
    Failed(&'a str),
}

pub fn look(row: &Row, now: Instant) -> Look {
    if row.discarding {
        return discarding(row);
    }
    match &row.task.status {
        Status::Running => running(row, Amount::of(row).unwrap_or_default(), now),
        Status::Paused => stopped(row, Stopped::Paused),
        Status::Failed(message) => stopped(row, Stopped::Failed(message)),
        Status::Done => done_look(row),
    }
}

/// 正在放弃：刻度按已有的内容、不发光，等它删完。
fn discarding(row: &Row) -> Look {
    let (done, total) = Amount::of(row).map_or((0, 0), |a| (a.done, a.total));
    Look {
        state: "放弃中".to_owned(),
        color: theme::muted(),
        ticks: Ticks {
            color: theme::finished(),
            glow: false,
            ..Ticks::of(total, done, false)
        },
        big: "…".to_owned(),
        line: "停下并删除任务目录".to_owned(),
        fraction: 0.0,
        orb: theme::finished(),
        short: "…".to_owned(),
    }
}

/// 暂停或失败的任务：按任务目录里已完成的内容显示，刻度不发光；失败的大字是「!」、说明是错误。点了继续、还在读
/// 任务目录时状态写「正在继续」。
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
            let (big, line) = paused_text(row, amount.as_ref(), recording);
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
                fraction,
                orb: theme::cyan_dim(),
            }
        }
        Stopped::Failed(message) => Look {
            state: state("需要处理"),
            color: theme::orange(),
            ticks: ticks(theme::finished()),
            big: "!".to_owned(),
            line: message.to_owned(),
            fraction,
            orb: theme::orange(),
            short: "!".to_owned(),
        },
    }
}

/// 暂停的任务的大字与说明：录制写已录的时长，点播写百分比与片数；没有内容时说明为什么。
fn paused_text(row: &Row, amount: Option<&Amount>, recording: bool) -> (String, String) {
    let known = amount.is_some_and(|a| a.total > 0);
    let line = match (amount, &row.disk) {
        (Some(a), _) if recording => {
            format!(
                "已录 {} · {}",
                format::clock(a.duration),
                format::bytes(a.bytes)
            )
        }
        (Some(a), _) if a.total > 0 => format!("{} / {} 片 · 继续时接着下", a.done, a.total),
        (Some(a), _) if a.done > 0 => {
            format!(
                "已下 {} 片 · {} · 继续时接着下",
                a.done,
                format::bytes(a.bytes)
            )
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
        fraction: 1.0,
        orb: theme::finished(),
        short: "✓".to_owned(),
    }
}

fn running(row: &Row, amount: Amount, now: Instant) -> Look {
    let stage = row.progress.as_ref().map_or(Stage::Preparing, |p| p.stage);
    if stage == Stage::Writing {
        let writing = row.progress.as_ref().and_then(|p| p.writing);
        let (kind, count) = match writing {
            Some(w) => match (w.mp4, w.hls) {
                (Some(c), _) => ("写出 MP4", Some(c)),
                (None, Some(c)) => ("写出本地 HLS", Some(c)),
                (None, None) => ("写出", None),
            },
            None => ("写出", None),
        };
        let written = count.map_or(0.0, |c| {
            (c.done as f32 / c.total.max(1) as f32).clamp(0.0, 1.0)
        });
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
                || "不经网络".to_owned(),
                |c| format!("已写 {} / {} 个文件 · 不经网络", c.done, c.total),
            ),
            fraction: written,
            orb: theme::cyan_pale(),
            short: percent,
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
    let mut line = format!(
        "{} / {} 片 · {}/s",
        amount.done,
        amount.total,
        format::bytes(row.rate.bytes_per_second(now) as u64)
    );
    if let Some(left) = row
        .rate
        .remaining(now, amount.total.saturating_sub(amount.done))
    {
        line.push_str(" · ");
        line.push_str(&format::remaining(left));
    }
    Look {
        state: "下载中".to_owned(),
        color: theme::cyan(),
        ticks,
        big: percent(amount.fraction()),
        line,
        fraction: amount.fraction(),
        orb: theme::cyan(),
        short: percent(amount.fraction()),
    }
}

/// 完成时的说明：输出的种类、视频的宽高与大小（`MP4 · 1920×1080 · 1.4 GB`）。
pub fn finished_text(output: &OutputSpec, result: &Finished) -> String {
    let kind = match output {
        OutputSpec::Mp4(_) => "MP4",
        OutputSpec::Hls(_) => "本地 HLS",
        OutputSpec::Both { .. } => "MP4 与本地 HLS",
    };
    let mut parts = vec![kind.to_owned()];
    if let Some((width, height)) = result.resolution {
        parts.push(format!("{width}×{height}"));
    }
    parts.push(format::bytes(result.bytes));
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use hs_m3u8_app::{StoredPreference, TaskId, TaskSpec};
    use hs_m3u8_core::hls::Preference;
    use hs_m3u8_core::{FileCount, SegmentCounts, Writing};

    use super::*;

    fn row(status: Status, live: bool, total: Option<usize>) -> Row {
        let task = Task {
            id: TaskId(1),
            spec: TaskSpec {
                title: "talk".into(),
                url: "https://a.example/talk.m3u8".parse().unwrap(),
                preference: StoredPreference::try_from(&Preference::default()).unwrap(),
                headers: Vec::new(),
                output: OutputSpec::Mp4("/m/talk.mp4".into()),
                live,
            },
            status,
            work_dir: Some("/m/talk.hsdl".into()),
            created_at: 0,
            total,
            result: None,
        };
        Row::new(task, Instant::now())
    }

    fn saved(segments: usize, live: bool) -> Saved {
        Saved {
            live,
            segments,
            bytes: 4_000_000,
            duration: Duration::from_secs(40),
        }
    }

    /// 暂停的任务按任务目录里读到的与存储里的总片数显示；读之前、读不出、目录不在各有说法。
    #[test]
    fn paused_tasks_show_what_is_in_the_work_dir() {
        let mut paused = row(Status::Paused, false, Some(160));
        assert_eq!(look(&paused, Instant::now()).line, "正在读取任务目录");
        paused.disk = Disk::Unreadable("权限不够".into());
        assert_eq!(
            look(&paused, Instant::now()).line,
            "读不出任务目录：权限不够"
        );
        paused.disk = Disk::Read(None);
        assert!(
            look(&paused, Instant::now())
                .line
                .starts_with("任务目录不在")
        );
        paused.disk = Disk::Read(Some(saved(40, false)));
        let seen = look(&paused, Instant::now());
        assert_eq!((seen.big.as_str(), seen.fraction), ("25%", 0.25));
        assert_eq!((seen.ticks.count, seen.ticks.lit), (160, 40));
        paused.task.total = None;
        assert_eq!(
            look(&paused, Instant::now()).line,
            "已下 40 片 · 4.0 MB · 继续时接着下"
        );
    }

    /// 继续后在准备阶段就失败：这次运行的进度是 0/0，读到任务目录后按目录里已有的显示。
    #[test]
    fn a_resume_that_fails_early_still_shows_the_work_dir() {
        let mut failed = row(Status::Failed("HTTP 状态 403".into()), false, Some(420));
        failed.progress = Some(Progress::default());
        failed.disk = Disk::Read(Some(saved(143, false)));
        let seen = look(&failed, Instant::now());
        assert_eq!((seen.ticks.count, seen.ticks.lit), (360, 122));
        assert_eq!(seen.big, "!");
    }

    /// 暂停的直播录制写已录的时长，不写片数与百分比。
    #[test]
    fn paused_recordings_show_recorded_time() {
        let mut paused = row(Status::Paused, true, None);
        paused.disk = Disk::Read(Some(saved(40, true)));
        let seen = look(&paused, Instant::now());
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
        let seen = look(&writing, Instant::now());
        assert_eq!(
            (seen.state.as_str(), seen.big.as_str()),
            ("写出 MP4", "99%")
        );
        assert_eq!((seen.ticks.lit, seen.ticks.written), (160, 159));
    }

    /// 按直播新建、实际走了点播的：进度到了下载阶段就按点播显示（可暂停），不再当作录制。
    #[test]
    fn a_live_task_that_turned_out_to_be_vod_is_not_recording() {
        let mut task = row(Status::Running, true, None);
        assert!(task.recording());
        task.progress = Some(Progress {
            stage: Stage::Downloading,
            ..Progress::default()
        });
        assert!(!task.recording());
    }

    #[test]
    fn finished_tasks_describe_their_output() {
        let mut done = row(Status::Done, false, Some(10));
        done.task.result = Some(Finished {
            duration: Some(Duration::from_secs(48 * 60 + 12)),
            bytes: 1_420_000_000,
            resolution: Some((1920, 1080)),
        });
        let seen = look(&done, Instant::now());
        assert_eq!((seen.ticks.count, seen.ticks.lit), (60, 60));
        assert_eq!(seen.big, "48:12");
        assert_eq!(seen.line, "MP4 · 1920×1080 · 1.4 GB");
    }
}
