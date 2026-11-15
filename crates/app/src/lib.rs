//! 桌面应用的业务层：任务与设置的存储、任务的生命周期与节流后的进度，建在 hs-m3u8-core（HLS 与单个文件）与
//! hs-dlx-torrent（BT）之上，不依赖界面框架。

pub mod curl;
mod error;
mod failure;
mod naming;
mod preference;
mod service;
mod settings;
pub mod special_link;
mod store;
mod task;
mod torrent;

pub use error::Error;
pub use failure::{Failure, FailureKind, not_a_playlist};
pub use naming::{file_name, task_title, title_of};
pub use preference::StoredPreference;
pub use service::{Event, Events, MetadataFetch, Service};
pub use settings::{Seeding, Settings, Theme};
pub use store::Store;
pub use task::{
    Draft, DraftKind, Expected, FileSpec, Finished, HlsSpec, Kind, OutputKind, OutputSpec, Status,
    Task, TaskId, TaskSpec, TorrentSpec,
};
pub use torrent::{
    MetadataProgress, SavedTorrent, SeedEnd, TorrentFile, TorrentInfo, TorrentNetwork,
    TorrentPhase, TorrentProgress, torrent_from_file,
};
