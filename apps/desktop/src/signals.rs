//! 任务的「信号」：比较前后两次进度，得出这期间新发生的、值得告诉人的事。

use std::collections::BTreeMap;

use hs_m3u8_core::{FetchErrorKind, FetchFailure, HttpErrorKind, Progress, Purpose, Stage};

/// 一条信号的分量，决定卡片的描边颜色。
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

/// 记下一条信号：与最后一条的分量和文字都相同时只加次数、更新时刻。
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
        let text = match (now.stage, now.segments.done) {
            (Stage::Recording, _) => "开始录制".to_owned(),
            (_, 0) => format!("开始下载 · {} 片", now.segments.total),
            (_, done) => format!(
                "续传：任务目录里已有 {done} 片，共 {} 片",
                now.segments.total
            ),
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
    // 同一时刻常有一批请求因同一原因重试：按（请求的是什么、原因）合成一条
    let retried_before = before.map_or(&[][..], |b| b.retrying.as_slice());
    let mut retries: BTreeMap<(&'static str, String), usize> = BTreeMap::new();
    for retry in now.retrying.iter().filter(|r| !retried_before.contains(r)) {
        *retries
            .entry((purpose(retry.purpose), failure(&retry.failure)))
            .or_default() += 1;
    }
    for ((what, why), count) in retries {
        let text = if count == 1 {
            format!("{what}{why}，重试中")
        } else {
            format!("{count} 个{what}{why}，重试中")
        };
        found.push((Level::Warn, text));
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

/// 失败原因的简短说法；HTTP 库给的完整错误链太长，放不进信号卡片。
fn failure(failure: &FetchFailure) -> String {
    match failure.kind() {
        FetchErrorKind::Http(kind) => match kind {
            HttpErrorKind::Status(code) => format!(" HTTP {code}"),
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
    use std::time::Duration;

    use hs_m3u8_core::{Retrying, SegmentCounts};

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

    #[test]
    fn the_start_is_reported_once() {
        let first = progress(Stage::Downloading, 0, 160);
        assert_eq!(
            between(None, &first),
            [(Level::Info, "开始下载 · 160 片".to_owned())]
        );
        let later = progress(Stage::Downloading, 3, 160);
        assert_eq!(between(Some(&first), &later), []);
        let preparing = progress(Stage::Preparing, 0, 0);
        assert_eq!(between(None, &preparing), []);
        let resumed = progress(Stage::Downloading, 40, 160);
        assert_eq!(
            between(Some(&preparing), &resumed),
            [(
                Level::Info,
                "续传：任务目录里已有 40 片，共 160 片".to_owned()
            )]
        );
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
                (Level::Warn, "3 个分片内容没写完，重试中".to_owned()),
            ]
        );
        // 同一次重试再报一遍不算新的
        assert_eq!(between(Some(&now), &now), []);

        let mut writing = now.clone();
        writing.stage = Stage::Writing;
        writing.retrying.clear();
        assert_eq!(
            between(Some(&now), &writing),
            [(Level::Info, "10 片下载完，开始写出".to_owned())]
        );
    }

    #[test]
    fn repeated_signals_are_counted_not_listed() {
        let mut signals = Vec::new();
        push(
            &mut signals,
            "10:00:00".into(),
            Level::Warn,
            "分片超时，重试中".into(),
        );
        push(
            &mut signals,
            "10:00:01".into(),
            Level::Warn,
            "分片超时，重试中".into(),
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
}
