//! 应用的设置，与任务一起存在任务存储里。

use std::path::PathBuf;

use hs_m3u8_core::{Proxy, Url};

use crate::Error;

/// 应用的设置。
#[derive(Clone, PartialEq, Eq)]
pub struct Settings {
    /// 新建任务默认保存到的目录（绝对路径）；没设过时为 None，由界面取系统的默认目录
    pub folder: Option<PathBuf>,
    /// 探测与开始任务时请求经的代理；任务开始后不随设置改变，暂停再继续后才换
    pub proxy: Proxy,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            folder: None,
            proxy: Proxy::System,
        }
    }
}

/// 代理在存储里的写法：`system`、`direct`，或 `url:<代理地址>`（明文，可带口令，与请求头一样只有本人可读写）。
/// 存储格式的一部分，表上有 CHECK。
pub(crate) fn proxy_code(proxy: &Proxy) -> String {
    match proxy {
        Proxy::System => "system".to_owned(),
        Proxy::Direct => "direct".to_owned(),
        Proxy::Url(url) => format!("url:{url}"),
    }
}

/// 读回 [`proxy_code`] 的写法；认不出时报 [`Error::Corrupt`]。
pub(crate) fn parse_proxy(code: &str) -> Result<Proxy, Error> {
    match code {
        "system" => Ok(Proxy::System),
        "direct" => Ok(Proxy::Direct),
        _ => code
            .strip_prefix("url:")
            .and_then(|url| Url::parse(url).ok())
            .map(Proxy::Url)
            .ok_or_else(|| Error::Corrupt("设置里的代理".to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxies_read_back_as_written() {
        for proxy in [
            Proxy::System,
            Proxy::Direct,
            Proxy::Url(Url::parse("socks5h://user:pass@127.0.0.1:1080").unwrap()),
        ] {
            assert!(parse_proxy(&proxy_code(&proxy)).unwrap() == proxy);
        }
        assert!(matches!(parse_proxy("pac:x"), Err(Error::Corrupt(_))));
    }
}
