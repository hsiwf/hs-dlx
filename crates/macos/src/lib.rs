//! GPUI 没开放、桌面应用又要用的 macOS 应用级接口。别的平台上这个 crate 是空的。

#[cfg(target_os = "macos")]
mod terminate;

#[cfg(target_os = "macos")]
pub use terminate::{Error, Reply, ask_before_terminate, install, reply_to_terminate};
