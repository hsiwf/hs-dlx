//! 什么文字算链接：粘贴栏、剪贴板与启动参数里的。

use hs_m3u8_core::Url;

/// `text`（去掉首尾空白）是 http 或 https 链接时为它。
pub fn http_url(text: &str) -> Option<Url> {
    Url::parse(text.trim())
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
}

/// 剪贴板里的文字是 http(s) 的 m3u8 链接时为它。
pub fn stream_url(text: &str) -> Option<Url> {
    http_url(text).filter(|url| url.path().to_ascii_lowercase().ends_with(".m3u8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_links_are_taken() {
        assert!(http_url(" https://a.example/x \n").is_some());
        assert!(http_url("file:///tmp/x.m3u8").is_none());
        assert!(http_url("看这个 https://a.example/x.m3u8").is_none());
        assert!(stream_url("https://a.example/v/index.m3u8?t=1").is_some());
        assert!(stream_url("https://a.example/v/INDEX.M3U8").is_some());
        assert!(stream_url("https://a.example/v/clip.mp4").is_none());
    }
}
