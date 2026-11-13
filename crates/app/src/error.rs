//! 业务层的错误。

use crate::TaskId;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 调用方给的值不合要求（如没法存的选轨偏好、不是 UTF-8 的输出路径）
    #[error("参数不合要求：{0}")]
    InvalidInput(String),
    /// 任务存储读写失败（盘满、文件被删、权限）；照原样报出，不可重试与否看包着的原因
    #[error("任务存储出错：{0}")]
    Store(#[from] rusqlite::Error),
    /// 任务存储里的内容认不出：本应用写入的都能读回，读不回即文件被改动过或被别的程序写过
    #[error("任务存储里的内容认不出：{0}")]
    Corrupt(String),
    #[error("没有{0}")]
    NotFound(TaskId),
    /// 任务正在运行，这个操作要等它停下
    #[error("{0}正在运行")]
    Running(TaskId),
    /// 任务正在放弃（停下、删任务目录），不能再继续或移除
    #[error("{0}正在放弃")]
    Discarding(TaskId),
    /// 只从列表移除（不删任务目录）只用于已完成的任务
    #[error("{0}没有完成，要连同任务目录一起放弃")]
    NotFinished(TaskId),
    /// 新任务的任务目录与存储里已有的任务相同
    #[error("任务目录已被另一个任务使用：{}", .0.display())]
    WorkDirTaken(std::path::PathBuf),
    /// 任务存储已被另一个运行中的应用打开；两个应用同时改同一份任务会互相覆盖状态
    #[error("任务存储已被另一个运行中的 hs-m3u8 打开：{}", .0.display())]
    InUse(std::path::PathBuf),
    /// 核心库报的错误
    #[error(transparent)]
    Core(#[from] hs_m3u8_core::Error),
    /// 文件系统或系统资源的操作失败：建存储所在的目录、设权限、给存储加锁、取名时检查路径是否已存在、启动 tokio
    /// 运行时；照原样报出
    #[error("{action} {path} 失败：{cause}")]
    Io {
        action: &'static str,
        path: std::path::PathBuf,
        cause: std::io::Error,
    },
}
