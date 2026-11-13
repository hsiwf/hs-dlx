//! 一次运行的下载速度与剩余时间的估计；时刻都由调用方给出。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 速度按最近这么久里读到的字节数算；这么久没有新数据即为 0。
const WINDOW: Duration = Duration::from_secs(3);
/// 开始不到这么久时不估剩余时间：分片常成批完成，刚开始的速度不可用。
const SETTLE: Duration = Duration::from_secs(2);

pub struct Rate {
    /// 本次运行开始的时刻
    started: Instant,
    /// 开始时已完成的分片数：新建为 0，继续时为任务目录里已有的；没读出来时为 None，不估剩余时间
    baseline: Option<usize>,
    /// (时刻, 本次运行读到的字节数)，按先后；只留时间窗内的与窗口前的最后一个
    samples: VecDeque<(Instant, u64)>,
    /// 最近一次进度里已完成的分片数
    done: usize,
}

impl Rate {
    /// 在 `now` 开始的一次运行，开始时已完成 `baseline` 片（不知道时为 None）。
    pub fn new(now: Instant, baseline: Option<usize>) -> Rate {
        Rate {
            started: now,
            baseline,
            samples: VecDeque::new(),
            done: 0,
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
