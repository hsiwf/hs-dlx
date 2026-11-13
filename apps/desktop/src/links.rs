//! 什么文字算链接：粘贴栏、剪贴板、启动参数与再次打开应用时转交来的。

use hs_m3u8_core::Url;

/// 不是 http(s) 链接时给用户的说法。
pub const NOT_HTTP: &str = "要 http 或 https 开头的链接";

/// `text`（去掉首尾空白）是 http 或 https 链接时为它。
pub fn http_url(text: &str) -> Option<Url> {
    Url::parse(text.trim())
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
}

/// `text` 是路径以 `.m3u8` 结尾的 http(s) 链接时为它：看着像 HLS 播放列表（剪贴板里的提示、探测失败时先做什么，
/// 只按地址猜；不是这样的地址也可能是播放列表）。
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
