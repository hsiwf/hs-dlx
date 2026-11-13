//! 任务失败的种类：把核心的错误翻译成界面要分开处理的几类，随失败的状态存进存储。

use hs_m3u8_core::remux::Error as RemuxError;
use hs_m3u8_core::{
    Error as CoreError, FetchFailure, HttpErrorKind, RefreshReason, StallReason, WorkDirProblem,
};

/// 任务以失败结束：种类与说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    /// 给用户看的说明（核心错误的文字）
    pub message: String,
}

impl Failure {
    /// 核心报的错误。
    pub fn of(error: &CoreError) -> Failure {
        Failure {
            kind: FailureKind::of(error),
            message: error.to_string(),
        }
    }
}

/// 失败的种类：调用方据此决定怎么提示、能不能重试、该不该先改请求头。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// 服务器回了这个 HTTP 状态码（如 403：多要带上 Referer、Cookie 之类的请求头）
    Status(u16),
    /// 连不上、超时、传输中断、内容没写完，或代理出错
    Network,
    /// 服务器的证书不受信任
    Certificate,
    /// 读写本机的文件出错（含写 MP4 时），或读不到所在的盘
    Disk,
    /// 输出路径、或要用的任务目录上已有别的东西
    OutputTaken,
    /// 来源与任务目录里已下载的不是同一份内容（换了内容、换了一档、轨道变了、直播接不上之前录的）
    SourceChanged,
    /// 取到的不是播放列表、分片或 key 该有的样子：多为源站返回了错误页或登录页
    Malformed,
    /// 直播长时间录不到新分片
    Stalled,
    /// 来源用了不支持的特性，或内容放不进 MP4
    Unsupported,
    /// 其余
    Other,
}

impl FailureKind {
    pub fn of(error: &CoreError) -> FailureKind {
        match error {
            CoreError::Http { kind, .. } => FailureKind::of_http(kind),
            CoreError::Segment { cause, .. } | CoreError::Key { cause, .. } => {
                FailureKind::of(cause)
            }
            CoreError::Cleanup { failure, .. } => FailureKind::of(failure),
            CoreError::LiveStalled { cause, .. } => match cause {
                // 刷新或下载一直被服务器拒绝时按状态码
                StallReason::RefreshFailed(RefreshReason::Http(failure))
                | StallReason::Unrecordable(FetchFailure::Http(failure)) => match failure.kind {
                    HttpErrorKind::Status(code) => FailureKind::Status(code),
                    _ => FailureKind::Stalled,
                },
                StallReason::RefreshFailed(_)
                | StallReason::RefreshPending
                | StallReason::Unrecordable(_)
                | StallReason::TrackStopped(_) => FailureKind::Stalled,
            },
            CoreError::Incomplete { .. } | CoreError::SystemProxy(_) => FailureKind::Network,
            CoreError::Io { .. } | CoreError::VolumeUnavailable { .. } => FailureKind::Disk,
            CoreError::Remux(error) => match error.as_ref() {
                RemuxError::Io { .. } | RemuxError::OpenOutput { .. } => FailureKind::Disk,
                RemuxError::Unsupported(_) => FailureKind::Unsupported,
                // 其余是合并本身出的错
                _ => FailureKind::Other,
            },
            CoreError::OutputExists(_) | CoreError::OutputOccupied { .. } => {
                FailureKind::OutputTaken
            }
            CoreError::WorkDir { problem, .. } => match problem {
                WorkDirProblem::NotEmpty => FailureKind::OutputTaken,
                WorkDirProblem::KindMismatch { .. }
                | WorkDirProblem::PreferenceChanged
                | WorkDirProblem::TracksMismatch
                | WorkDirProblem::PlanChanged
                | WorkDirProblem::SelectionGone
                | WorkDirProblem::SourceUnverified
                | WorkDirProblem::Unaligned { .. } => FailureKind::SourceChanged,
                WorkDirProblem::Locked
                | WorkDirProblem::Corrupt(_)
                | WorkDirProblem::UnsupportedVersion { .. }
                | WorkDirProblem::NotLiveRecording => FailureKind::Other,
            },
            CoreError::Select(_) => FailureKind::SourceChanged,
            CoreError::Playlist { .. }
            | CoreError::KeyLength { .. }
            | CoreError::ResponseTooLarge { .. }
            | CoreError::Integrity { .. } => FailureKind::Malformed,
            CoreError::Unsupported(_) | CoreError::NotMediaPlaylist { .. } => {
                FailureKind::Unsupported
            }
            CoreError::InvalidInput(_)
            | CoreError::LiveNotEnabled
            | CoreError::NothingRecorded
            | CoreError::Hook { .. }
            | CoreError::HttpClientInit(_)
            | CoreError::Cancelled => FailureKind::Other,
        }
    }

    fn of_http(kind: &HttpErrorKind) -> FailureKind {
        match kind {
            HttpErrorKind::Status(code) => FailureKind::Status(*code),
            HttpErrorKind::Timeout
            | HttpErrorKind::Connect(_)
            | HttpErrorKind::Transport(_)
            | HttpErrorKind::Proxy(_) => FailureKind::Network,
            HttpErrorKind::Certificate(_) => FailureKind::Certificate,
            HttpErrorKind::RangeIgnored(_)
            | HttpErrorKind::RangeMismatch(_)
            | HttpErrorKind::Redirect(_)
            | HttpErrorKind::InvalidRequest(_) => FailureKind::Other,
        }
    }

    /// 什么都不改、再试一次有没有可能成功：状态码只有 408、429 与 5xx 是暂时的；证书、内容不对、来源变了、不支持的
    /// 再试也一样；磁盘、输出已存在、网络与停滞的，外面的情况会变（腾出空间、挪走文件、网络恢复）。
    pub fn retry_helps(self) -> bool {
        match self {
            FailureKind::Status(code) => matches!(code, 408 | 429 | 500..=599),
            FailureKind::Network
            | FailureKind::Disk
            | FailureKind::OutputTaken
            | FailureKind::Stalled
            | FailureKind::Other => true,
            FailureKind::Certificate
            | FailureKind::SourceChanged
            | FailureKind::Malformed
            | FailureKind::Unsupported => false,
        }
    }

    /// 多半要先改请求头（补上 Referer、Cookie、Authorization）：被服务器拒绝，或取到的是错误页、登录页。
    pub fn headers_may_help(self) -> bool {
        matches!(
            self,
            FailureKind::Status(401 | 403) | FailureKind::Malformed
        )
    }

    /// 在存储里的写法：`status:<状态码>` 或种类名。存储格式的一部分：改写法或加种类都要加一项存储迁移（存储里有
    /// CHECK 列出全部写法，旧版本应用打不开新写法的存储，而不是读出认不出的行）。
    pub(crate) fn code(self) -> String {
        match self {
            FailureKind::Status(code) => format!("status:{code}"),
            FailureKind::Network => "network".into(),
            FailureKind::Certificate => "certificate".into(),
            FailureKind::Disk => "disk".into(),
            FailureKind::OutputTaken => "output-taken".into(),
            FailureKind::SourceChanged => "source-changed".into(),
            FailureKind::Malformed => "malformed".into(),
            FailureKind::Stalled => "stalled".into(),
            FailureKind::Unsupported => "unsupported".into(),
            FailureKind::Other => "other".into(),
        }
    }

    /// [`FailureKind::code`] 的反过来；认不出时为 None。
    pub(crate) fn parse(code: &str) -> Option<FailureKind> {
        Some(match code {
            "network" => FailureKind::Network,
            "certificate" => FailureKind::Certificate,
            "disk" => FailureKind::Disk,
            "output-taken" => FailureKind::OutputTaken,
            "source-changed" => FailureKind::SourceChanged,
            "malformed" => FailureKind::Malformed,
            "stalled" => FailureKind::Stalled,
            "unsupported" => FailureKind::Unsupported,
            "other" => FailureKind::Other,
            _ => FailureKind::Status(code.strip_prefix("status:")?.parse().ok()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hs_m3u8_core::{HttpFailure, StallReason, Url};

    use super::*;

    fn http(kind: HttpErrorKind) -> CoreError {
        CoreError::Http {
            url: Box::new(Url::parse("https://a.example/x.m3u8").unwrap()),
            kind,
            retry_after: None,
        }
    }

    /// 分片里包着的 403、直播刷新一直 403 都按状态码；其余按种类。
    #[test]
    fn errors_are_classified_through_their_causes() {
        let segment = CoreError::Segment {
            track: 0,
            sequence: 7,
            url: Box::new(Url::parse("https://a.example/7.ts").unwrap()),
            cause: Box::new(http(HttpErrorKind::Status(403))),
        };
        assert_eq!(FailureKind::of(&segment), FailureKind::Status(403));
        let stalled = CoreError::LiveStalled {
            track: 0,
            cause: StallReason::RefreshFailed(RefreshReason::Http(HttpFailure {
                kind: HttpErrorKind::Status(401),
                retry_after: None,
            })),
        };
        assert_eq!(FailureKind::of(&stalled), FailureKind::Status(401));
        assert_eq!(
            FailureKind::of(&http(HttpErrorKind::Timeout)),
            FailureKind::Network
        );
        assert_eq!(
            FailureKind::of(&CoreError::OutputExists(PathBuf::from("/m/a.mp4"))),
            FailureKind::OutputTaken
        );
    }

    /// 再试也一样的不给重试；被拒或取到错误页的要先改请求头。
    #[test]
    fn kinds_tell_whether_retrying_or_headers_help() {
        assert!(FailureKind::Status(503).retry_helps());
        assert!(!FailureKind::Status(404).retry_helps());
        assert!(!FailureKind::Malformed.retry_helps());
        assert!(FailureKind::Malformed.headers_may_help());
        assert!(FailureKind::Status(403).headers_may_help());
        assert!(!FailureKind::Network.headers_may_help());
        let unaligned = CoreError::WorkDir {
            path: PathBuf::from("/m/a.hsdl"),
            problem: WorkDirProblem::Unaligned { track: 1 },
        };
        assert_eq!(FailureKind::of(&unaligned), FailureKind::SourceChanged);
    }

    #[test]
    fn codes_round_trip() {
        for kind in [
            FailureKind::Status(403),
            FailureKind::Network,
            FailureKind::Certificate,
            FailureKind::Disk,
            FailureKind::OutputTaken,
            FailureKind::SourceChanged,
            FailureKind::Malformed,
            FailureKind::Stalled,
            FailureKind::Unsupported,
            FailureKind::Other,
        ] {
            assert_eq!(FailureKind::parse(&kind.code()), Some(kind));
        }
        assert_eq!(FailureKind::parse("status:x"), None);
        assert_eq!(FailureKind::parse("gone"), None);
    }
}
