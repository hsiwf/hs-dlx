//! 任务的「信号」：比较前后两次进度，得出这期间新发生的、值得告诉人的事。

use std::collections::BTreeMap;
use std::time::Duration;

use hs_m3u8_core::{
    FetchErrorKind, FetchFailure, HttpErrorKind, Progress, Purpose, Retrying, Stage,
};

use crate::format;

/// 一条信号的级别，决定卡片的描边颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    /// 重试等自己会恢复的问题
    Warn,
    /// 任务因此停下
    Error,
}

/// 一条带时刻的信号；与上一条相同的不另起一条，只加 `repeat`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    /// 最近一次发生的本地时刻
    pub at: String,
    pub level: Level,
    pub text: String,
    /// 连续发生了几次，至少 1
    pub repeat: usize,
}

/// 每个任务最多留多少条，旧的先丢。
pub const KEEP: usize = 50;

/// 记下一条信号：与最后一条的级别和文字都相同时只加次数、更新时刻。
pub fn push(signals: &mut Vec<Signal>, at: String, level: Level, text: String) {
    if let Some(last) = signals.last_mut()
        && last.level == level
        && last.text == text
    {
        last.repeat += 1;
        last.at = at;
        return;
    }
    signals.push(Signal {
        at,
        level,
        text,
        repeat: 1,
    });
    if signals.len() > KEEP {
        signals.remove(0);
    }
}

/// 从 `before`（这次运行里上一次的进度；刚开始时为 None）到 `now` 之间新发生的事。
pub fn between(before: Option<&Progress>, now: &Progress) -> Vec<(Level, String)> {
    let mut found = Vec::new();
    let known_total = before.is_some_and(|b| b.segments.total > 0);
    if !known_total && now.segments.total > 0 {
        let quality = now
            .selected
            .as_ref()
            .map(|s| format!(" · {}", format::variant(&s.variant)))
            .unwrap_or_default();
        let text = match now.stage {
            Stage::Recording => format!("开始录制{quality}"),
            _ => format!("开始下载{quality} · {} 片", now.segments.total),
        };
        found.push((Level::Info, text));
    }
    let gaps_before = before.map_or(0, |b| b.segments.gap);
    if now.segments.gap > gaps_before {
        found.push((
            Level::Info,
            format!("{} 片来源标为空缺，不下载", now.segments.gap - gaps_before),
        ));
    }
    // 新开始退避的：按多重集合比较，同一批里完全相同的几条各算一次。
    // 简化：重试里没有是哪个请求，一条退避结束重试、另一条完全相同的同时开始时，后一条不报；要准确得让核心带上请求身份。
    let mut unmatched: Vec<&Retrying> =
        before.map_or_else(Vec::new, |b| b.retrying.iter().collect());
    let mut fresh = Vec::new();
    for retry in &now.retrying {
        match unmatched.iter().position(|old| *old == retry) {
            Some(at) => {
                unmatched.swap_remove(at);
            }
            None => fresh.push(retry),
        }
    }
    // 同一时刻常有一批请求因同一原因重试：按（请求的是什么、原因）合成一条
    let mut groups: BTreeMap<(&'static str, String), Vec<&Retrying>> = BTreeMap::new();
    for retry in fresh {
        groups
            .entry((purpose(retry.purpose), failure(&retry.failure)))
            .or_default()
            .push(retry);
    }
    for ((what, why), retries) in groups {
        let delay = retries.iter().map(|r| r.delay).min().expect("每组至少一条");
        let text = match retries.as_slice() {
            [one] => format!(
                "{}，{}后第 {} 次重试",
                joined(what, &why),
                seconds(delay),
                one.retry
            ),
            many => format!(
                "{} 个{}，{}后重试",
                many.len(),
                joined(what, &why),
                seconds(delay)
            ),
        };
        found.push((Level::Warn, text));
    }
    // 直播：另起会话与缺片。缺片按本次运行计，新的运行（`before` 为 None）从 0 起
    if let (Some(old), Some(new)) = (before.and_then(|b| b.session), now.session)
        && new != old
    {
        found.push((Level::Warn, format!("来源接不上，另起会话 {}", new + 1)));
    }
    let failed_before = before.map_or(0, |b| b.segments.failed);
    if now.segments.failed > failed_before {
        found.push((
            Level::Warn,
            format!("{} 片取不到，录不回来", now.segments.failed - failed_before),
        ));
    }
    let expired_before = before.map_or(0, |b| b.segments.expired);
    if now.segments.expired > expired_before {
        found.push((
            Level::Warn,
            format!(
                "{} 片在刷新前已滑出窗口，录不回来",
                now.segments.expired - expired_before
            ),
        ));
    }
    if now.stage == Stage::Writing && before.is_none_or(|b| b.stage != Stage::Writing) {
        found.push((
            Level::Info,
            format!("{} 片下载完，开始写出", now.segments.done),
        ));
    }
    found
}

fn purpose(purpose: Purpose) -> &'static str {
    match purpose {
        Purpose::Playlist => "播放列表",
        Purpose::Key => "密钥",
        Purpose::Init => "init 段",
        Purpose::Segment => "分片",
    }
}

/// 「请求的是什么」与「原因」连成一句：原因以字母或数字开头时（`HTTP 503`）中间空一格。
fn joined(what: &str, why: &str) -> String {
    if why.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        format!("{what} {why}")
    } else {
        format!("{what}{why}")
    }
}

/// 退避时长：不足 10 秒保留一位小数。
fn seconds(delay: Duration) -> String {
    let secs = delay.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.1} 秒")
    } else {
        format!("{secs:.0} 秒")
    }
}

/// 失败原因的简短说法；HTTP 库给的完整错误链太长，放不进信号卡片。
fn failure(failure: &FetchFailure) -> String {
    match failure.kind() {
        FetchErrorKind::Http(kind) => match kind {
            HttpErrorKind::Status(code) => format!("HTTP {code}"),
            HttpErrorKind::Timeout => "超时".to_owned(),
            HttpErrorKind::Connect(_) => "连不上服务器".to_owned(),
            HttpErrorKind::Transport(_) => "传输中断".to_owned(),
            HttpErrorKind::Certificate(_) => "证书校验失败".to_owned(),
            HttpErrorKind::Redirect(_) => "重定向失败".to_owned(),
            HttpErrorKind::Proxy(_) => "被代理拒绝".to_owned(),
            HttpErrorKind::RangeIgnored(_) | HttpErrorKind::RangeMismatch(_) => {
                "字节范围不符".to_owned()
            }
            HttpErrorKind::InvalidRequest(_) => "请求不合法".to_owned(),
        },
        FetchErrorKind::Incomplete => "内容没写完".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use hs_m3u8_core::SegmentCounts;

    use super::*;

    fn progress(stage: Stage, done: usize, total: usize) -> Progress {
        Progress {
            stage,
            segments: SegmentCounts {
                done,
                total,
                ..SegmentCounts::default()
            },
            ..Progress::default()
        }
    }

    fn retry(retry: u32) -> Retrying {
        Retrying {
            purpose: Purpose::Segment,
            failure: FetchFailure::Incomplete,
            retry,
            delay: Duration::from_millis(500),
        }
    }

    /// 核心先发出准备阶段（总片数未知），下载开始的那次更新给出总片数；续传前已有的片数之后才计入，不影响这条。
    #[test]
    fn the_start_is_reported_once() {
        let preparing = progress(Stage::Preparing, 0, 0);
        assert_eq!(between(None, &preparing), []);
        let first = progress(Stage::Downloading, 0, 160);
        assert_eq!(
            between(Some(&preparing), &first),
            [(Level::Info, "开始下载 · 160 片".to_owned())]
        );
        let resumed = progress(Stage::Downloading, 40, 160);
        assert_eq!(between(Some(&first), &resumed), []);
    }

    #[test]
    fn gaps_retries_and_writing_are_reported_when_they_appear() {
        let before = progress(Stage::Downloading, 10, 160);
        let mut now = before.clone();
        now.segments.gap = 2;
        now.retrying = vec![retry(1), retry(2), retry(3)];
        assert_eq!(
            between(Some(&before), &now),
            [
                (Level::Info, "2 片来源标为空缺，不下载".to_owned()),
                (Level::Warn, "3 个分片内容没写完，0.5 秒后重试".to_owned()),
            ]
        );
        // 同一次重试再报一遍不算新的；与已有的完全相同的又来一条，算一条新的
        assert_eq!(between(Some(&now), &now), []);
        let mut again = now.clone();
        again.retrying.push(retry(1));
        assert_eq!(
            between(Some(&now), &again),
            [(
                Level::Warn,
                "分片内容没写完，0.5 秒后第 1 次重试".to_owned()
            )]
        );

        let mut writing = now.clone();
        writing.stage = Stage::Writing;
        writing.retrying.clear();
        assert_eq!(
            between(Some(&now), &writing),
            [(Level::Info, "10 片下载完，开始写出".to_owned())]
        );
    }

    /// 直播另起会话、本次运行新缺的片各报一次；新的运行（之前没有进度）不把编号当作变化。
    #[test]
    fn new_sessions_and_misses_are_reported() {
        let mut before = progress(Stage::Recording, 10, 10);
        before.session = Some(0);
        let mut now = before.clone();
        now.session = Some(1);
        now.segments.failed = 2;
        now.segments.expired = 45;
        let texts: Vec<String> = between(Some(&before), &now)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(
            texts,
            [
                "来源接不上，另起会话 2",
                "2 片取不到，录不回来",
                "45 片在刷新前已滑出窗口，录不回来"
            ]
        );
        assert_eq!(between(Some(&now), &now), []);
        let mut resumed = progress(Stage::Recording, 10, 10);
        resumed.session = Some(3);
        assert!(
            between(None, &resumed)
                .iter()
                .all(|(_, t)| !t.contains("会话"))
        );
    }

    #[test]
    fn repeated_signals_are_counted_not_listed() {
        let mut signals = Vec::new();
        push(
            &mut signals,
            "10:00:00".into(),
            Level::Warn,
            "分片超时".into(),
        );
        push(
            &mut signals,
            "10:00:01".into(),
            Level::Warn,
            "分片超时".into(),
        );
        push(
            &mut signals,
            "10:00:02".into(),
            Level::Info,
            "开始写出".into(),
        );
        assert_eq!(signals.len(), 2);
        assert_eq!((signals[0].repeat, signals[0].at.as_str()), (2, "10:00:01"));
        assert_eq!(signals[1].repeat, 1);
    }

    #[test]
    fn reasons_read_as_one_phrase() {
        assert_eq!(joined("分片", "HTTP 503"), "分片 HTTP 503");
        assert_eq!(joined("密钥", "超时"), "密钥超时");
    }
}
