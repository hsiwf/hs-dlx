//! 注入网页的脚本与它发回的消息。脚本在页面与它的各层框架里找 m3u8 地址、数请求、报告页面地址与标题；发回的
//! 内容来自网页，按不可信数据校验，认不出的丢掉。

use hs_m3u8_core::Url;
use serde::Deserialize;

use crate::links::{http_url, stream_url};

/// 注入每一层框架的脚本：经 fetch、XHR、资源加载记录与 video 元素找 m3u8 地址（每个只报一次），每秒最多报一次请求
/// 数；顶层框架另报页面的地址与标题（加载时与地址、标题变化时）。发回的是 JSON，见 [`Report`]。
pub const SCRIPT: &str = r#"
(function () {
  if (window.__hsM3u8Sniff) return;
  window.__hsM3u8Sniff = true;
  var post = function (message) {
    try { window.ipc.postMessage(JSON.stringify(message)); } catch (e) {}
  };
  var seen = new Set();
  var requests = 0, reported = -1;
  var found = function (raw) {
    if (!raw) return;
    var url;
    try { url = new URL(String(raw), location.href).href; } catch (e) { return; }
    if (!/\.m3u8(\?|#|$)/i.test(url) || seen.has(url)) return;
    seen.add(url);
    post({ kind: 'stream', url: url, page: location.href, agent: navigator.userAgent });
  };
  var fetch0 = window.fetch;
  if (fetch0) {
    window.fetch = function (input) {
      requests += 1;
      found(typeof input === 'string' ? input : input && input.url);
      return fetch0.apply(this, arguments);
    };
  }
  var open0 = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url) {
    requests += 1;
    found(url);
    return open0.apply(this, arguments);
  };
  try {
    new PerformanceObserver(function (list) {
      list.getEntries().forEach(function (entry) { requests += 1; found(entry.name); });
    }).observe({ type: 'resource', buffered: true });
  } catch (e) {}
  var top = window === window.top;
  var page = '', title = '';
  setInterval(function () {
    document.querySelectorAll('video, source').forEach(function (v) { found(v.currentSrc || v.src); });
    if (requests !== reported) { reported = requests; post({ kind: 'requests', frame: location.href, count: requests }); }
    if (top && (location.href !== page || document.title !== title)) {
      page = location.href; title = document.title;
      post({ kind: 'page', url: page, title: title });
    }
  }, 1000);
})();
"#;

/// 一条消息最长多少字节：页面能发任意多、任意长的消息，超过的不解析。
const MAX_MESSAGE: usize = 16 * 1024;
/// 标题、User-Agent 与框架地址最长多少字符，超出的截掉。
const MAX_TEXT: usize = 300;

/// 脚本发回、校验过的一条消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// 找到一个 m3u8 地址：`page` 为它所在框架的地址（请求时作 Referer），`agent` 为网页的 User-Agent
    Stream { url: Url, page: Url, agent: String },
    /// 框架 `frame`（它的地址，只用来区分各层框架）到现在发出的请求数
    Requests { frame: String, count: u32 },
    /// 顶层页面的地址与标题
    Page { url: Url, title: String },
}

/// 消息的原样内容，未校验。
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Raw {
    Stream {
        url: String,
        page: String,
        agent: String,
    },
    Requests {
        frame: String,
        count: u32,
    },
    Page {
        url: String,
        title: String,
    },
}

/// 校验一条消息；太长、不是认得的 JSON、地址不是 http(s)（m3u8 地址还要以 `.m3u8` 结尾）的为 None。
pub fn parse(message: &str) -> Option<Report> {
    if message.len() > MAX_MESSAGE {
        return None;
    }
    Some(match serde_json::from_str(message).ok()? {
        Raw::Stream { url, page, agent } => Report::Stream {
            url: stream_url(&url)?,
            page: http_url(&page)?,
            agent: clip(&agent),
        },
        Raw::Requests { frame, count } => Report::Requests {
            frame: clip(&frame),
            count,
        },
        Raw::Page { url, title } => Report::Page {
            url: http_url(&url)?,
            title: clip(&title),
        },
    })
}

/// 去掉控制字符、最多留 [`MAX_TEXT`] 个字符。
fn clip(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_checked_before_use() {
        let stream = r#"{"kind":"stream","url":"https://cdn.example/a/index.m3u8?t=1",
            "page":"https://video.example/p/3","agent":"Mozilla/5.0"}"#;
        assert_eq!(
            parse(stream),
            Some(Report::Stream {
                url: Url::parse("https://cdn.example/a/index.m3u8?t=1").unwrap(),
                page: Url::parse("https://video.example/p/3").unwrap(),
                agent: "Mozilla/5.0".into(),
            })
        );
        // 不是 m3u8、不是 http(s)、认不出的种类、太长的都不要
        assert_eq!(
            parse(r#"{"kind":"stream","url":"https://a/x.mp4","page":"https://a/","agent":""}"#),
            None
        );
        assert_eq!(
            parse(r#"{"kind":"page","url":"javascript:alert(1)","title":"x"}"#),
            None
        );
        assert_eq!(parse(r#"{"kind":"run","command":"rm"}"#), None);
        assert_eq!(parse(&"x".repeat(MAX_MESSAGE + 1)), None);
        let page = format!(
            r#"{{"kind":"page","url":"https://a/","title":"{}\u0007"}}"#,
            "长".repeat(400)
        );
        let Some(Report::Page { title, .. }) = parse(&page) else {
            panic!("页面消息应认得");
        };
        assert_eq!(title.chars().count(), MAX_TEXT);
    }
}
