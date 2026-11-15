//! 应用的设置，与任务一起存在任务存储里。

use std::path::PathBuf;

use hs_m3u8_core::Proxy;

/// 应用的设置。
#[derive(Clone, PartialEq, Eq)]
pub struct Settings {
    /// 新建任务默认保存到的目录（绝对路径）；没设过时为 None，由界面取系统的默认目录
    pub folder: Option<PathBuf>,
    /// 探测与开始任务时请求经的代理；任务开始后不随设置改变，暂停再继续后才换
    pub proxy: Proxy,
    pub theme: Theme,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            folder: None,
            proxy: Proxy::System,
            theme: Theme::Eclipse,
        }
    }
}

/// 外观主题：窗口背景的光与界面的强调色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    /// 日蚀：青色的光，外缘偏深蓝
    Eclipse,
    /// 血月：火红的光，外缘偏暗红
    BloodMoon,
}
