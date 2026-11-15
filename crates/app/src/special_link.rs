//! 下载工具的专用链接：迅雷 `thunder://`、QQ 旋风 `qqdl://`、快车 `flashget://`，把真实地址按各自的写法包了一层
//! base64：
//! - `thunder://` + base64(`AA` + 地址 + `ZZ`)
//! - `flashget://` + base64(`[FLASHGET]` + 地址 + `[FLASHGET]`)，后面可能接着 `&` 与站点附加的标记
//! - `qqdl://` + base64(地址)
//!
//! base64 末尾的 `=` 可有可无，末尾多出的 `/` 去掉。里面的地址要是 UTF-8：早年的链接有用 GBK 写中文文件名的，这里
//! 认不出，报错而不猜编码。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT;

/// 专用链接解不出地址。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecialLinkError {
    /// 值为链接的前缀（如 `thunder://`）
    #[error("{0} 链接里不是 base64，可能没复制全")]
    NotBase64(&'static str),
    #[error("{0} 链接里的地址不是 UTF-8（可能是 GBK 写的中文），换用原始链接")]
    NotUtf8(&'static str),
    #[error("{0} 链接里的内容不是这种链接的写法")]
    Unwrapped(&'static str),
}

/// 各种专用链接：前缀（小写）与包在地址前后的标记。
const KINDS: [(&str, &str, &str); 3] = [
    ("thunder://", "AA", "ZZ"),
    ("flashget://", "[FLASHGET]", "[FLASHGET]"),
    ("qqdl://", "", ""),
];

/// `text`（去掉首尾空白）是专用链接时为解出的地址，或解不出的原因；不是专用链接时为 None。解出的地址不检查协议。
pub fn unwrap(text: &str) -> Option<Result<String, SpecialLinkError>> {
    let text = text.trim();
    let (prefix, head, tail) = KINDS.into_iter().find(|(prefix, _, _)| {
        text.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    })?;
    let body = &text[prefix.len()..];
    // 快车链接的 `&` 之后是站点附加的标记，不属于 base64
    let body = body.split('&').next().unwrap_or_default();
    let body = body.trim_end_matches('/');
    Some(decode(prefix, body, head, tail))
}

fn decode(
    prefix: &'static str,
    body: &str,
    head: &str,
    tail: &str,
) -> Result<String, SpecialLinkError> {
    let bytes = STANDARD_PAD_INDIFFERENT
        .decode(body)
        .map_err(|_| SpecialLinkError::NotBase64(prefix))?;
    let inner = String::from_utf8(bytes).map_err(|_| SpecialLinkError::NotUtf8(prefix))?;
    inner
        .strip_prefix(head)
        .and_then(|rest| rest.strip_suffix(tail))
        .map(str::to_owned)
        .ok_or(SpecialLinkError::Unwrapped(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(prefix: &str, inner: &str) -> String {
        format!(
            "{prefix}{}",
            base64::engine::general_purpose::STANDARD.encode(inner)
        )
    }

    #[test]
    fn the_three_kinds_unwrap() {
        let url = "https://a.example/视频/1.mp4?x=1";
        assert_eq!(
            unwrap(&wrap("thunder://", &format!("AA{url}ZZ"))),
            Some(Ok(url.to_owned()))
        );
        assert_eq!(
            unwrap(&format!(
                "  {}&1926/ ",
                wrap("FlashGet://", &format!("[FLASHGET]{url}[FLASHGET]"))
            )),
            Some(Ok(url.to_owned()))
        );
        // 去掉了 base64 末尾的 `=`
        let bare = wrap("qqdl://", "http://a.example/");
        assert!(bare.ends_with('='));
        assert_eq!(
            unwrap(bare.trim_end_matches('=')),
            Some(Ok("http://a.example/".to_owned()))
        );
        assert_eq!(unwrap("https://a.example/"), None);
    }

    #[test]
    fn broken_links_say_why() {
        assert_eq!(
            unwrap("thunder://不是base64"),
            Some(Err(SpecialLinkError::NotBase64("thunder://")))
        );
        assert_eq!(
            unwrap(&wrap("thunder://", "http://a.example/x")),
            Some(Err(SpecialLinkError::Unwrapped("thunder://")))
        );
        let gbk = format!(
            "thunder://{}",
            base64::engine::general_purpose::STANDARD.encode(b"AAhttp://a/\xca\xd3ZZ")
        );
        assert_eq!(
            unwrap(&gbk),
            Some(Err(SpecialLinkError::NotUtf8("thunder://")))
        );
    }
}
