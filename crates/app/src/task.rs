//! 任务：用户新建的一次下载或录制，及其在存储里的状态。

use std::path::PathBuf;

use hs_m3u8_core::hls::Preference;
use hs_m3u8_core::{OutputOptions, Target, Url};

use crate::Failure;
use crate::preference::StoredPreference;

/// 输出要哪几种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Mp4,
    Hls,
    Both,
}

/// 任务存储里的编号，新建时由 SQLite 分配。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub i64);

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "任务 {}", self.0)
    }
}

/// 新建任务时用户给定的：输出路径由服务按它取（见 [`crate::Service::create`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub url: Url,
    /// 视频所在网页的标题，任务的标题与主名照它取（见 [`crate::task_title`]）；None 时由地址得出
    pub title: Option<String>,
    /// 选轨偏好；按下标选的存不了（主播放列表变了下标就不是同一条），报 [`crate::Error::InvalidInput`]
    pub preference: Preference,
    /// 额外的请求头
    pub headers: Vec<(String, String)>,
    pub output: OutputKind,
    /// 输出放在哪个目录，须是绝对路径
    pub folder: PathBuf,
    /// 来源是直播时录制；点播时不起作用
    pub live: bool,
}

/// 新建任务时给定的内容，续传与续录照它重建请求；之后只有请求头可在任务停着时改（[`crate::Service::set_headers`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpec {
    /// 列表里显示的名字
    pub title: String,
    pub url: Url,
    pub preference: StoredPreference,
    /// 额外的请求头，原样存储（含凭据时同样明文，存储文件只有本人可读写）
    pub headers: Vec<(String, String)>,
    pub output: OutputSpec,
    /// 来源是直播时录制；点播时不起作用
    pub live: bool,
}

/// 输出：路径都是以文件名结尾的绝对路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputSpec {
    Mp4(PathBuf),
    Hls(PathBuf),
    Both { mp4: PathBuf, hls: PathBuf },
}

impl OutputSpec {
    /// 核心的输出选项，任务目录取核心的默认位置（与输出同级的 `<主名>.hsdl`）。新建任务时由此定下任务目录，记在
    /// [`Task::work_dir`]，之后照它指定。
    pub fn options(&self) -> OutputOptions {
        OutputOptions::new(match self.clone() {
            OutputSpec::Mp4(path) => Target::Mp4(path),
            OutputSpec::Hls(path) => Target::Hls(path),
            OutputSpec::Both { mp4, hls } => Target::Both { mp4, hls },
        })
    }
}

/// 任务在存储里的状态。运行中的细节（进度、阶段）不存，见 [`crate::Event`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// 在下载或录制
    Running,
    /// 用户暂停；任务目录保留，继续时续传。应用异常退出时还在运行的，重开后也是这个状态
    Paused,
    /// 以错误结束；任务目录保留
    Failed(Failure),
    /// 已写出输出
    Done,
    /// 放弃了而没做完（中途出错、超时或应用退出）：任务目录可能已删了一部分，不能继续，只能再放弃
    DiscardUnfinished,
}

/// 存储里的一个任务。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: TaskId,
    pub spec: TaskSpec,
    pub status: Status,
    /// 任务目录（绝对路径），新建时定下、之后不变。None 时任务没有自己的任务目录：旧版本存储里已完成的，或与别的任务
    /// 共用任务目录、迁移时让给了那个任务的（状态为失败）；这样的任务不能继续，只能从列表移除
    pub work_dir: Option<PathBuf>,
    /// 新建时间，Unix 秒
    pub created_at: i64,
    /// 点播的总片数（各轨合计），第一次运行拿到播放列表后记下，用于任务没在运行时显示进度；直播与还没拿到时为 None
    pub total: Option<usize>,
    /// 完成时写出的结果；没完成时为 None
    pub result: Option<Finished>,
    /// 用户选了不再续录（[`crate::Service::write_recorded`]）：再运行时只写出已录的，直到用
    /// [`crate::Service::resume`] 续录
    pub write_only: bool,
}

/// 完成时写出的结果，供之后显示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// MP4 里视频流的时长，没有 MP4 或没有视频流时为第 0 条轨分片的声明时长之和；都没有时为 None
    pub duration: Option<std::time::Duration>,
    /// 写出的分片与 init 段的字节数（解密后）
    pub bytes: u64,
    /// MP4 里视频流的宽与高；没有时为 None
    pub resolution: Option<(u32, u32)>,
}
