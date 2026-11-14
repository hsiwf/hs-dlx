//! 任务列表里的一行：存储里的任务，加上这次运行收到的进度、结果、信号与速度。界面上的样子见 `crate::shell` 的 look。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{Local, TimeZone as _};
use hs_m3u8_app::{Finished, OutputSpec, Status, Task};
use hs_m3u8_core::{Progress, Saved, Stage};

use crate::format;
use crate::live::Timeline;
use crate::rate::Rate;
use crate::signals::{self, Level, Signal};

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
    /// 开始只写出已录的那一刻的已录时长：那次运行在核心报出进度之前按它显示
    pub recorded_before_writing: Duration,
}

impl Row {
    pub fn new(task: Task, now: Instant) -> Row {
        // 存储里的时间是本应用写的 Unix 秒；换不成本地时刻（超出 chrono 的范围）时不写时刻
        let at = Local
            .timestamp_opt(task.created_at, 0)
            .earliest()
            .map_or_else(|| "—".to_owned(), |t| t.format("%m-%d %H:%M").to_string());
        let file_name = file_name(&task.spec.output);
        let mut signals = Vec::new();
        signals::push(
            &mut signals,
            at,
            Level::Info,
            format!("新建 · 保存为 {file_name}"),
        );
        // 打开应用时就已失败的：完整的错误只在存储里，补进信号
        if let Status::Failed(failure) = &task.status {
            signals::push(
                &mut signals,
                "上次".to_owned(),
                Level::Error,
                failure.message.clone(),
            );
        }
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
            recorded_before_writing: Duration::ZERO,
        }
    }

    /// 换上改后的输出（改存本地 HLS 之后）。
    pub fn set_output(&mut self, output: OutputSpec) {
        self.file_name = file_name(&output);
        self.task.spec.output = output;
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

    /// 能停止并写出：在运行的直播录制（还在准备的也算：会话定下之前停止即结束，写出已录的）。点播停止不起作用，要暂停。
    pub fn stoppable(&self) -> bool {
        self.task.status == Status::Running && self.recording()
    }

    /// 作废读到的任务目录内容（任务停下时：内容变了），之后发起的读才算数。
    pub fn forget_disk(&mut self) {
        self.disk = Disk::Unread;
        self.reading += 1;
    }

    /// 正在写出时写出的比例（0 到 1，按写出的文件数）；不在写出阶段时为 None。
    pub fn written_share(&self) -> Option<f32> {
        let progress = self.progress.as_ref()?;
        if progress.stage != Stage::Writing {
            return None;
        }
        let writing = progress.writing?;
        let count = writing.mp4.or(writing.hls);
        Some(count.map_or(0.0, |c| {
            (c.done as f32 / c.total.max(1) as f32).clamp(0.0, 1.0)
        }))
    }

    /// 在运行的点播此刻的进度（0 到 1）：写出时为写出的比例，下载时为已下的片数比例（还不知道合计时为 0）。不是在运行
    /// 的点播（停着的、录直播的、只写出已录的）时为 None。
    pub fn download_share(&self) -> Option<f32> {
        if self.task.status != Status::Running || self.task.write_only || self.recording() {
            return None;
        }
        Some(
            self.written_share()
                .unwrap_or_else(|| Amount::of(self).map_or(0.0, |amount| amount.fraction())),
        )
    }

    /// 是直播的录制：运行中过了准备阶段的按阶段；其余（停下的、还在准备的）按任务目录里的记录（继续前读到的），
    /// 没读到时按新建时的选择。按直播新建、实际是点播的，进度到了下载阶段或读到任务目录后就不算。
    pub fn recording(&self) -> bool {
        if self.task.status == Status::Running && self.task.write_only {
            return false;
        }
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

/// 已完成、合计（不知道时为 0）、字节数与第 0 条轨的时长。
#[derive(Default)]
pub struct Amount {
    pub done: usize,
    /// 不知道时为 0
    pub total: usize,
    pub bytes: u64,
    pub duration: Duration,
}

impl Amount {
    /// 运行中取这次运行的进度；停下的取任务目录里读到的（与存储里的总片数），还没读到时取这次运行留下的进度。
    pub fn of(row: &Row) -> Option<Amount> {
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

    /// 已完成的比例（0 到 1）；不知道合计时为 0。
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f32 / self.total as f32).min(1.0)
        }
    }
}

/// 输出的文件名（同时要 MP4 与本地 HLS 时为 MP4 的）。
fn file_name(output: &OutputSpec) -> String {
    let target = output.options().target;
    target
        .mp4()
        .or(target.hls())
        .expect("输出至少有 MP4 与本地 HLS 之一")
        .file_name()
        .expect("存储读回任务时已校验输出路径以文件名结尾")
        .to_string_lossy()
        .into_owned()
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

/// 测试用的行与任务目录里的内容。
#[cfg(test)]
pub mod sample {
    use hs_m3u8_app::{OutputSpec, Status, StoredPreference, Task, TaskId, TaskSpec};
    use hs_m3u8_core::Saved;
    use hs_m3u8_core::hls::Preference;
    use std::time::{Duration, Instant};

    use super::Row;

    pub fn row(status: Status, live: bool, total: Option<usize>) -> Row {
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
            write_only: false,
        };
        Row::new(task, Instant::now())
    }

    pub fn saved(segments: usize, live: bool) -> Saved {
        Saved {
            live,
            segments,
            bytes: 4_000_000,
            duration: Duration::from_secs(40),
        }
    }
}

#[cfg(test)]
mod tests {
    use hs_m3u8_app::Status;
    use hs_m3u8_core::{Progress, Stage};

    use super::sample::row;

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
}
