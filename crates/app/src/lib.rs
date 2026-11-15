//! 桌面应用的业务层：任务与设置的存储、任务的生命周期与节流后的进度，建在 hs-m3u8-core 之上，不依赖界面框架。

pub mod curl;
mod error;
mod failure;
mod naming;
mod preference;
mod service;
mod settings;
mod store;
mod task;

pub use error::Error;
pub use failure::{Failure, FailureKind, not_a_playlist};
pub use naming::{task_title, title_of};
pub use preference::StoredPreference;
pub use service::{Event, Events, Service};
pub use settings::Settings;
pub use store::Store;
pub use task::{Draft, Finished, OutputKind, OutputSpec, Status, Task, TaskId, TaskSpec};
