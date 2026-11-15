//! 什么文字算链接：粘贴栏、剪贴板、启动参数与再次打开应用时转交来的。

use hs_dlx_app::curl;
use hs_m3u8_core::Url;

/// 不是 http(s) 链接时给用户的说法。
pub const NOT_HTTP: &str = "要 http 或 https 开头的链接";

/// `text`（去掉首尾空白）是 http 或 https 链接时为它。
pub fn http_url(text: &str) -> Option<Url> {
    Url::parse(text.trim())
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
}

/// 要探测的请求：`text` 是 curl 命令时按命令拆（见 [`curl::parse`]），否则要是 http(s) 链接（不带请求头）；都不是时
/// 为给用户的原因。
pub fn request(text: &str) -> Result<curl::Request, String> {
    if curl::is_command(text) {
        return curl::parse(text).map_err(|error| error.to_string());
    }
    let url = http_url(text).ok_or_else(|| NOT_HTTP.to_owned())?;
    Ok(curl::Request {
        url,
        headers: Vec::new(),
        dropped: Vec::new(),
    })
}

/// `text` 作为要探测的请求时的地址；不是链接也不是认得出的 curl 命令时为 None。
pub fn target(text: &str) -> Option<Url> {
    request(text).ok().map(|request| request.url)
}

/// `text` 是路径以 `.m3u8` 结尾的 http(s) 链接时为它：看着像 HLS 播放列表（剪贴板里的提示、取到的不是播放列表时要不要
/// 看它是不是网页，只按地址猜；不是这样的地址也可能是播放列表）。
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
