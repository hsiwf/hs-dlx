//! 装饰动效的节拍：粘贴栏描边的流光、主按钮的呼吸、在下的那一格刻度的闪烁、任务栏小环上绕行的光点、信号里最新
//! 一条的呼吸。都按同一个时钟算，取的是窗口重画那一刻的值；重画的节奏由主窗口的定时器定（窗口在前台时每秒约 30
//! 次）。系统要求减少动态效果时（[`set_still`]）都停在固定的样子。

use std::f32::consts::TAU;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 时钟的起点：第一次取节拍的时刻。
static EPOCH: OnceLock<Instant> = OnceLock::new();
/// 系统要求减少动态效果。
static STILL: AtomicBool = AtomicBool::new(false);

/// 记下系统是不是要求减少动态效果。
pub fn set_still(still: bool) {
    STILL.store(still, Ordering::Relaxed);
}

/// 时钟走了多少秒；停着时为 None。
fn seconds() -> Option<f32> {
    (!STILL.load(Ordering::Relaxed))
        .then(|| EPOCH.get_or_init(Instant::now).elapsed().as_secs_f32())
}

/// 闪烁的亮度，0.2 到 1，约每秒一次；停着时为 1。
pub fn blink() -> f32 {
    seconds().map_or(1., |t| 0.6 + 0.4 * (t * 6.).sin())
}

/// 呼吸的强度，一个周期 `period` 里由 1 降到 0.65 再回到 1；停着时为 1。
pub fn breathe(period: Duration) -> f32 {
    seconds().map_or(1., |t| {
        let x = (t / period.as_secs_f32()).fract();
        0.825 + 0.175 * (TAU * x).cos()
    })
}

/// 转一圈要 `period` 时此刻转到哪（0 到 1）；停着时为 None（不画绕行的东西）。
pub fn turn(period: Duration) -> Option<f32> {
    seconds().map(|t| (t / period.as_secs_f32()).fract())
}
