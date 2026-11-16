//! BT 的公共 tracker 列表（ngosang/trackerslist 的 trackers_best_ip.txt）：补给不是私有的种子，多一条找节点的路，DHT
//! 不通的网络里尤其要紧。列表在 BT 会话建起时开始取，取不到隔一阵再取，取到为止；补的不随种子存下（见
//! [`hs_dlx_torrent::Session::add_trackers`]），每次加入会话时补当时的列表。

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use hs_m3u8_core::{Engine, Proxy, Source, Url};

/// 列表的来源，依次试，取到一个即止：jsDelivr 的 CDN，再是 GitHub Pages 与 GitHub 的原始文件。用 IP 写的那份：
/// tracker 的域名在有的网络里解析不出或被污染。
pub(crate) const SOURCES: [&str; 3] = [
    "https://cdn.jsdelivr.net/gh/ngosang/trackerslist@master/trackers_best_ip.txt",
    "https://ngosang.github.io/trackerslist/trackers_best_ip.txt",
    "https://raw.githubusercontent.com/ngosang/trackerslist/master/trackers_best_ip.txt",
];

/// 列表最多多少字节：实际约 1 KB，超过即不是这份列表。
const LIMIT: u64 = 64 << 10;
/// 每个来源最多试几次、每次最多多久：连不上的来源尽快换下一个，外面还有隔一阵再取的退避。
const ATTEMPTS: NonZeroU32 = NonZeroU32::new(2).unwrap();
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 没取到时第一次隔多久再取，之后每次翻倍，最多隔 [`LAST_RETRY`]。
pub(crate) const FIRST_RETRY: Duration = Duration::from_secs(60);
pub(crate) const LAST_RETRY: Duration = Duration::from_secs(30 * 60);
/// 报错时最多引用列表里一行的多少个字符：取到的不是列表时（如网页）那一行可能很长。
const QUOTED: usize = 80;

/// 公共 tracker 列表的状况。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PublicTrackers {
    /// 不补：没有列表的来源（只用于测试）
    Off,
    /// 还没开始取：BT 会话还没建
    Idle,
    Fetching,
    /// 取到了：tracker 的地址，去重、保持列表里的顺序，至少一个
    Ready(Arc<[String]>),
    /// 这次没取到，为原因（各来源的原因以「；」相连）；隔一阵再取
    Failed(String),
}

/// 从 `sources` 依次取列表，经代理 `proxy`，每个来源最多试 [`ATTEMPTS`] 次、每次最多 [`REQUEST_TIMEOUT`]；都没取到时
/// 为各来源的原因。
pub(crate) async fn fetch(
    engine: &Engine,
    sources: &[Url],
    proxy: &Proxy,
) -> Result<Vec<String>, String> {
    let mut failures = Vec::new();
    for url in sources {
        let mut source = Source::new(url.clone());
        source.http.proxy = proxy.clone();
        source.http.retry.attempts = ATTEMPTS;
        source.http.timeouts.request = REQUEST_TIMEOUT;
        let fetched = match engine.fetch(&source, LIMIT).await {
            Ok(fetched) => parse(&fetched.body).map_err(|why| format!("{url} 取到的{why}")),
            Err(error) => Err(error.to_string()),
        };
        match fetched {
            Ok(trackers) => return Ok(trackers),
            Err(why) => failures.push(why),
        }
    }
    Err(failures.join("；"))
}

/// 读出列表：UTF-8 的文字，每行一个 tracker 的地址（udp、http 或 https，有主机），空行不算；去重，保持顺序。有一行
/// 不是 tracker 的地址、或一个也没有时为原因：取到的不是这份列表（如出错的网页），整份不用。
pub(crate) fn parse(body: &[u8]) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(body).map_err(|_| "不是 UTF-8 的文字".to_owned())?;
    let mut trackers: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let tracker = Url::parse(line)
            .ok()
            .filter(|url| matches!(url.scheme(), "udp" | "http" | "https") && url.host().is_some());
        if tracker.is_none() {
            let quoted: String = line.chars().take(QUOTED).collect();
            let more = if quoted.len() < line.len() { "…" } else { "" };
            return Err(format!(
                "不是 tracker 列表：「{quoted}{more}」不是 tracker 的地址"
            ));
        }
        if !trackers.iter().any(|known| known == line) {
            trackers.push(line.to_owned());
        }
    }
    if trackers.is_empty() {
        return Err("tracker 列表是空的".to_owned());
    }
    Ok(trackers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_read_line_by_line() {
        let body = b"udp://93.158.213.92:1337/announce\n\nhttp://211.75.205.187:80/announce\r\n\
                     udp://93.158.213.92:1337/announce\n";
        assert_eq!(
            parse(body).unwrap(),
            [
                "udp://93.158.213.92:1337/announce",
                "http://211.75.205.187:80/announce"
            ]
        );
    }

    #[test]
    fn anything_else_is_refused_whole() {
        assert!(parse(b"\n \n").unwrap_err().contains("空的"));
        assert!(parse(&[0xff, 0xfe]).unwrap_err().contains("UTF-8"));
        let page = format!(
            "udp://93.158.213.92:1337/announce\n<html>{}</html>",
            "x".repeat(200)
        );
        let why = parse(page.as_bytes()).unwrap_err();
        assert!(why.contains("<html>") && why.ends_with("…」不是 tracker 的地址"));
        assert!(parse(b"wss://tracker.example/announce").is_err());
        assert!(parse(b"udp:announce").is_err());
    }
}
