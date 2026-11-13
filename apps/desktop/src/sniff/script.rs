//! 注入网页每一层框架的脚本与它发回的消息。脚本找 HLS 播放列表的地址、数请求；发回的内容来自网页，按不可信数据校验，
//! 认不出的丢掉。页面的地址与标题不经脚本，由网页引擎报（见嗅探页）。

use hs_m3u8_core::Url;
use serde::Deserialize;

use crate::links::http_url;

/// 注入每一层框架的脚本。
///
/// - 播放列表：fetch 与 XHR 的响应类型是 HLS 播放列表（`application/vnd.apple.mpegurl` 之类）的，与地址路径以
///   `.m3u8` 结尾的请求（含资源加载记录与 video、source 元素上的），每个地址只报一次。
/// - 请求数：资源加载记录的条数，每秒最多报一次；框架按脚本随机取的编号区分。
/// - 顶层框架经 wry 的 `window.ipc` 发回 JSON（见 [`Report`]），并转发子框架经 `postMessage` 交来的（wry 只在顶层框架
///   提供 ipc），转发时附上浏览器给出的来源框架的 origin。
/// - `window.__hsM3u8Pause()`（只在顶层框架）暂停本框架的视频与音频，并让各层子框架也暂停。
pub const SCRIPT: &str = r#"
(function () {
  if (window.__hsM3u8Sniff) return;
  Object.defineProperty(window, '__hsM3u8Sniff', { value: true });
  var top = window === window.top;
  var frame = Math.random().toString(36).slice(2);
  var send = function (message) {
    if (top) {
      message.origin = location.origin;
      window.ipc.postMessage(JSON.stringify(message));
    } else {
      window.top.postMessage({ hsM3u8: message }, '*');
    }
  };
  if (top) {
    window.addEventListener('message', function (event) {
      var data = event.data;
      if (event.source === window || !data || typeof data.hsM3u8 !== 'object' || data.hsM3u8 === null) return;
      var message = data.hsM3u8;
      message.origin = event.origin;
      window.ipc.postMessage(JSON.stringify(message));
    });
  }
  var playlist = /^\s*(application\/(vnd\.apple\.mpegurl|x-mpegurl)|audio\/(x-)?mpegurl)\s*(;|$)/i;
  var named = function (raw) {
    var url;
    // 解析不了的不是地址，不算
    try { url = new URL(String(raw), location.href); } catch (e) { return false; }
    return /\.m3u8$/i.test(url.pathname);
  };
  var seen = new Set();
  var found = function (raw) {
    var url = new URL(String(raw), location.href).href;
    if (seen.has(url)) return;
    seen.add(url);
    send({ kind: 'stream', url: url, agent: navigator.userAgent });
  };
  var fetch0 = window.fetch;
  if (fetch0) {
    window.fetch = function (input) {
      var url = input instanceof Request ? input.url : String(input);
      if (named(url)) found(url);
      var pending = fetch0.apply(this, arguments);
      // 只看响应类型；请求失败由页面自己处理，这里另接一个处理只为不多出一个没人处理的失败
      pending.then(function (response) {
        if (playlist.test(response.headers.get('content-type') || '')) found(response.url);
      }, function () {});
      return pending;
    };
  }
  var open0 = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url) {
    if (named(url)) found(url);
    this.addEventListener('load', function () {
      if (playlist.test(this.getResponseHeader('content-type') || '')) found(this.responseURL);
    });
    return open0.apply(this, arguments);
  };
  var requests = 0, reported = -1;
  new PerformanceObserver(function (list) {
    list.getEntries().forEach(function (entry) {
      requests += 1;
      if (named(entry.name)) found(entry.name);
    });
  }).observe({ type: 'resource', buffered: true });
  var pause = function () {
    document.querySelectorAll('video, audio').forEach(function (media) { media.pause(); });
    for (var i = 0; i < window.frames.length; i += 1) window.frames[i].postMessage({ hsM3u8Pause: true }, '*');
  };
  window.addEventListener('message', function (event) {
    if (event.data && event.data.hsM3u8Pause === true) pause();
  });
  if (top) Object.defineProperty(window, '__hsM3u8Pause', { value: pause });
  setInterval(function () {
    document.querySelectorAll('video, source').forEach(function (media) {
      var src = media.currentSrc || media.src;
      if (src && named(src)) found(src);
    });
    if (requests !== reported) {
      reported = requests;
      send({ kind: 'requests', frame: frame, count: requests });
    }
  }, 1000);
})();
"#;

/// 在顶层框架里执行：暂停网页里的视频与音频（见 [`SCRIPT`]）。
pub const PAUSE: &str = "window.__hsM3u8Pause && window.__hsM3u8Pause();";

/// 一条消息最长多少字节：页面能发任意多、任意长的消息，超过的不解析。
const MAX_MESSAGE: usize = 16 * 1024;
/// User-Agent 与框架编号最长多少字符，超出的截掉。
const MAX_TEXT: usize = 300;

/// 脚本发回、校验过的一条消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// 找到一个播放列表的地址：`origin` 为它所在框架的 origin（不是 http(s) 的为 None，如沙箱里的框架），`agent` 为
    /// 网页的 User-Agent
    Stream {
        url: Url,
        origin: Option<Url>,
        agent: String,
    },
    /// 框架 `frame`（脚本随机取的编号）到现在发出的请求数
    Requests { frame: String, count: u32 },
}

/// 消息的原样内容，未校验。
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Raw {
    Stream {
        url: String,
        origin: String,
        agent: String,
    },
    Requests {
        frame: String,
        count: u32,
    },
}

/// 校验一条消息；太长、不是认得的 JSON、地址不是 http(s) 的为 None。
pub fn parse(message: &str) -> Option<Report> {
    if message.len() > MAX_MESSAGE {
        return None;
    }
    Some(match serde_json::from_str(message).ok()? {
        Raw::Stream { url, origin, agent } => Report::Stream {
            url: http_url(&url)?,
            origin: http_url(&origin),
            agent: clip(&agent),
        },
        Raw::Requests { frame, count } => Report::Requests {
            frame: clip(&frame),
            count,
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
        let stream = r#"{"kind":"stream","url":"https://cdn.example/a/playlist?id=3",
            "origin":"https://video.example","agent":"Mozilla/5.0"}"#;
        assert_eq!(
            parse(stream),
            Some(Report::Stream {
                url: Url::parse("https://cdn.example/a/playlist?id=3").unwrap(),
                origin: Some(Url::parse("https://video.example").unwrap()),
                agent: "Mozilla/5.0".into(),
            })
        );
        // 沙箱里的框架 origin 为 null：照收，不带 Referer
        let sandboxed = r#"{"kind":"stream","url":"https://a/x.m3u8","origin":"null","agent":""}"#;
        assert!(matches!(
            parse(sandboxed),
            Some(Report::Stream { origin: None, .. })
        ));
        // 不是 http(s)、认不出的种类、太长的都不要
        assert_eq!(
            parse(r#"{"kind":"stream","url":"javascript:alert(1)","origin":"null","agent":""}"#),
            None
        );
        assert_eq!(parse(r#"{"kind":"run","command":"rm"}"#), None);
        assert_eq!(parse(&"x".repeat(MAX_MESSAGE + 1)), None);
        let requests = format!(
            r#"{{"kind":"requests","frame":"{}\u0007","count":3}}"#,
            "长".repeat(400)
        );
        let Some(Report::Requests { frame, count }) = parse(&requests) else {
            panic!("请求数消息应认得");
        };
        assert_eq!((frame.chars().count(), count), (MAX_TEXT, 3));
    }
}
