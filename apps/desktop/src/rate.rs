//! 一次运行的下载速度与剩余时间的估计；时刻都由调用方给出。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 速度按最近这么久里读到的字节数算；这么久没有新数据即为 0。
const WINDOW: Duration = Duration::from_secs(3);
/// 开始不到这么久时不估剩余时间：分片常成批完成，刚开始的速度不可用。
const SETTLE: Duration = Duration::from_secs(2);
/// 速度表（[`Rate::history`]）一格管多久、一共几格：最近 60 秒。
pub const BAR: Duration = Duration::from_millis(1500);
pub const BARS: usize = 40;

pub struct Rate {
    /// 本次运行开始的时刻
    started: Instant,
    /// 开始时已完成的分片数：新建为 0，继续时为任务目录里已有的；没读出来时为 None，不估剩余时间
    baseline: Option<usize>,
    /// (时刻, 本次运行读到的字节数)，按先后；只留时间窗内的与窗口前的最后一个
    samples: VecDeque<(Instant, u64)>,
    /// 最近一次进度里已完成的分片数
    done: usize,
    /// 最近一次进度里本次运行读到的字节数
    received: u64,
    /// 速度表的格：(从开始算第几格, 这一格里读到的字节数)，按先后，只留最近的 [`BARS`] 格与正在走的一格；没读到字节的
    /// 格不记
    bars: VecDeque<(u64, u64)>,
}

impl Rate {
    /// 在 `now` 开始的一次运行，开始时已完成 `baseline` 片（不知道时为 None）。
    pub fn new(now: Instant, baseline: Option<usize>) -> Rate {
        Rate {
            started: now,
            baseline,
            samples: VecDeque::new(),
            done: 0,
            received: 0,
            bars: VecDeque::new(),
        }
    }

    /// 记一次进度：`now` 时本次运行读到 `received` 字节、已完成 `done` 片。
    pub fn update(&mut self, now: Instant, received: u64, done: usize) {
        self.samples.push_back((now, received));
        if let Some(edge) = now.checked_sub(WINDOW) {
            while self.samples.len() > 1 && self.samples[1].0 <= edge {
                self.samples.pop_front();
            }
        }
        self.done = done;
        let gained = received.saturating_sub(self.received);
        self.received = received;
        if gained > 0 {
            let bar = self.bar_of(now);
            match self.bars.back_mut() {
                Some((last, bytes)) if *last == bar => *bytes += gained,
                _ => self.bars.push_back((bar, gained)),
            }
            while self
                .bars
                .front()
                .is_some_and(|(first, _)| first + (BARS as u64) < bar)
            {
                self.bars.pop_front();
            }
        }
    }

    /// `now` 落在从开始算的第几格。
    fn bar_of(&self, now: Instant) -> u64 {
        (now.saturating_duration_since(self.started).as_millis() / BAR.as_millis()) as u64
    }

    /// `now` 之前已走完的最近 [`BARS`] 格（`now` 所在的那格还没走完，不算）的平均速度，字节每秒，旧的在前；开始之前
    /// 的格为 0。
    pub fn history(&self, now: Instant) -> [f64; BARS] {
        let current = self.bar_of(now);
        let mut history = [0.; BARS];
        for (slot, value) in history.iter_mut().enumerate() {
            let Some(bar) = (current + slot as u64).checked_sub(BARS as u64) else {
                continue;
            };
            if let Some((_, bytes)) = self.bars.iter().find(|(at, _)| *at == bar) {
                *value = *bytes as f64 / BAR.as_secs_f64();
            }
        }
        history
    }

    /// `now` 时的速度，字节每秒：时间窗起点（不晚于 `now` 前 [`WINDOW`] 的最后一次进度，没有时为第一次）到现在读到的
    /// 字节数除以经过的时间。最后一次进度已在时间窗之外（没有新数据，核心也就不发进度）时为 0。
    pub fn bytes_per_second(&self, now: Instant) -> f64 {
        let Some(&(last_at, latest)) = self.samples.back() else {
            return 0.0;
        };
        if now.saturating_duration_since(last_at) >= WINDOW {
            return 0.0;
        }
        let edge = now.checked_sub(WINDOW);
        let &(from, base) = self
            .samples
            .iter()
            .rev()
            .find(|(at, _)| edge.is_some_and(|edge| *at <= edge))
            .or(self.samples.front())
            .expect("至少有最后一次");
        let elapsed = now.saturating_duration_since(from).as_secs_f64();
        if elapsed <= 0.0 {
            return 0.0;
        }
        latest.saturating_sub(base) as f64 / elapsed
    }

    /// 还剩 `left` 片时估计的剩余时间：按本次运行以来的平均分片速度（不算开始时已有的）。起点不知道、开始不到
    /// [`SETTLE`]、或本次运行还没下完过分片（续传时已有的片数还没计入进度也算）时为 None。
    pub fn remaining(&self, now: Instant, left: usize) -> Option<Duration> {
        let finished = self.done.checked_sub(self.baseline?)?;
        let elapsed = now.saturating_duration_since(self.started);
        (elapsed >= SETTLE && finished > 0 && left > 0)
            .then(|| elapsed.mul_f64(left as f64 / finished as f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_follows_the_window_and_drops_to_zero_when_stalled() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut rate = Rate::new(t0, Some(0));
        assert_eq!(rate.bytes_per_second(t0), 0.0);
        rate.update(at(0), 0, 0);
        rate.update(at(1000), 1_000_000, 1);
        rate.update(at(2000), 2_000_000, 2);
        assert_eq!(rate.bytes_per_second(at(2000)), 1_000_000.0);
        // 停住、核心不再发进度：只推进时刻，窗口内按 1 秒时的起点算，最后一次进度出了窗口即为 0
        assert_eq!(rate.bytes_per_second(at(4000)), 1_000_000.0 / 3.0);
        assert_eq!(rate.bytes_per_second(at(5000)), 0.0);
        assert_eq!(rate.bytes_per_second(at(60_000)), 0.0);
        // 起点不知道时不估剩余时间
        let mut unknown = Rate::new(t0, None);
        unknown.update(at(3000), 0, 10);
        assert_eq!(unknown.remaining(at(3000), 10), None);
    }

    #[test]
    fn history_averages_finished_bars() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut rate = Rate::new(t0, Some(0));
        // 第 0 格读到 3 MB，第 1 格读到 1.5 MB，第 2 格没读到
        rate.update(at(500), 1_500_000, 1);
        rate.update(at(1000), 3_000_000, 2);
        rate.update(at(2000), 4_500_000, 3);
        let history = rate.history(at(4600));
        assert_eq!(history[BARS - 3], 2_000_000.0);
        assert_eq!(history[BARS - 2], 1_000_000.0);
        assert_eq!(history[BARS - 1], 0.0);
        assert!(history[..BARS - 3].iter().all(|&v| v == 0.0));
        // 还在第 0 格里时一格也没走完
        assert!(rate.history(at(1400)).iter().all(|&v| v == 0.0));
        // 第 40 格里读到 1.5 MB：速度表里最旧的仍是第 0 格
        rate.update(at(60_100), 6_000_000, 4);
        assert_eq!(rate.history(at(60_500))[0], 2_000_000.0);
        // 第 41 格时最旧的是第 1 格，第 0 格已移出速度表
        let later = rate.history(at(61_600));
        assert_eq!(later[0], 1_000_000.0);
        assert_eq!(later[BARS - 1], 1_000_000.0);
        assert_eq!(later.iter().filter(|&&v| v > 0.0).count(), 2);
    }

    #[test]
    fn remaining_time_counts_only_this_run() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        // 继续一个已下 400 片的任务：核心先报 0 片（已有的还没计入），再一次计入 400 片，都不估
        let mut rate = Rate::new(t0, Some(400));
        rate.update(at(1), 0, 0);
        assert_eq!(rate.remaining(at(3), 100), None);
        rate.update(at(3), 0, 400);
        assert_eq!(rate.remaining(at(3), 100), None);
        // 4 秒下了 8 片：剩下的 100 片约 50 秒
        rate.update(at(4), 0, 408);
        assert_eq!(rate.remaining(at(4), 100), Some(Duration::from_secs(50)));
        // 刚开始不估
        let mut early = Rate::new(t0, Some(0));
        early.update(at(1), 0, 32);
        assert_eq!(early.remaining(at(1), 128), None);
    }
}
