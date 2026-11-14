//! 列表里的一条流与它在后台探测到哪一步。

use hs_m3u8_core::{Probe, Url};

/// 一条流在后台探测到哪一步。
pub(super) enum Check {
    Waiting,
    Running,
    /// 探测成了：结果、探测时带的请求头与列表里的说明（几档清晰度、时长或直播）
    Ready {
        probe: Box<Probe>,
        headers: Vec<(String, String)>,
        summary: String,
    },
    /// 没成：原因与探测时带的请求头；没配出请求头（读不出 Cookie）时为 None
    Failed {
        reason: String,
        headers: Option<Vec<(String, String)>>,
    },
}

/// 找到的一条流。
pub(super) struct Found {
    /// 主机与路径（见 [`key`]）：同一个播放列表常以不同的查询参数反复请求（直播刷新、令牌），按它算一条
    pub key: String,
    /// 最近一次报来的地址；探测中与探测成了的不再换
    pub url: Url,
    /// 报它的框架的地址，网页引擎给的；认不出是哪个框架（Linux）或不是 http(s) 时为 None
    pub frame: Option<Url>,
    /// 报它时已显示的顶层页面的地址，网页引擎给的；不是 http(s) 时为 None
    pub top: Option<Url>,
    /// 网页的 User-Agent
    pub agent: String,
    pub check: Check,
    /// 配请求头时没带上的、放不进请求头的 Cookie 个数
    pub skipped: usize,
}

impl Found {
    pub fn new(url: Url, frame: Option<Url>, top: Option<Url>, agent: String) -> Found {
        Found {
            key: key(&url),
            url,
            frame,
            top,
            agent,
            check: Check::Waiting,
            skipped: 0,
        }
    }

    /// 同一条流又报来了（见 [`Found::key`]）：还没探测或没成的换成新报来的地址与来历，没成的重新排队（新地址常带着
    /// 新的令牌）。探测中与探测成了的不动。
    pub fn renew(&mut self, other: Found) {
        match self.check {
            Check::Waiting => {}
            Check::Failed { .. } if self.url != other.url => self.check = Check::Waiting,
            Check::Failed { .. } | Check::Running | Check::Ready { .. } => return,
        }
        self.url = other.url;
        self.frame = other.frame;
        self.top = other.top;
        self.agent = other.agent;
    }
}

/// 一条流的主机与路径，不含查询参数。
fn key(url: &Url) -> String {
    format!("{}{}", url.host_str().unwrap_or_default(), url.path())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Found {
        Found::new(Url::parse(text).unwrap(), None, None, String::new())
    }

    #[test]
    fn the_same_playlist_with_other_parameters_is_one_stream() {
        let mut first = found("https://cdn.example/live/index.m3u8?_HLS_msn=1");
        let again = found("https://cdn.example/live/index.m3u8?_HLS_msn=2");
        assert_eq!(first.key, again.key);
        // 还没探测的换成新地址
        first.renew(again);
        assert_eq!(first.url.query(), Some("_HLS_msn=2"));
        // 没成的换地址并重新排队
        first.check = Check::Failed {
            reason: "403".into(),
            headers: None,
        };
        first.renew(found("https://cdn.example/live/index.m3u8?token=new"));
        assert!(matches!(first.check, Check::Waiting));
        assert_eq!(first.url.query(), Some("token=new"));
        // 探测中的不动
        first.check = Check::Running;
        first.renew(found("https://cdn.example/live/index.m3u8?_HLS_msn=3"));
        assert_eq!(first.url.query(), Some("token=new"));
    }
}
