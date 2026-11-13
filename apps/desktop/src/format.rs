//! 界面上数字与路径的写法。

use std::path::Path;
use std::time::Duration;

/// 字节数：1000 进制，保留一位小数（`812 B`、`1.9 GB`）；四舍五入后到 1000 的进到下一级。
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while unit < UNITS.len() - 1 && (value * 10.0).round() / 10.0 >= 1000.0 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 时长：不足一小时写 `分:秒`，否则 `时:分:秒`。
pub fn clock(duration: Duration) -> String {
    let total = duration.as_secs();
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// 剩余时间：只写最大的两级（`剩余 42 秒`、`剩余 3 分 10 秒`、`剩余 1 小时 2 分`）。
pub fn remaining(duration: Duration) -> String {
    let total = duration.as_secs();
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("剩余 {h} 小时 {m} 分")
    } else if m > 0 {
        format!("剩余 {m} 分 {s} 秒")
    } else {
        format!("剩余 {s} 秒")
    }
}

/// 码率：bit/s 写成 Mbps 或 kbps，保留一位小数。
pub fn bitrate(bits_per_second: u64) -> String {
    if bits_per_second >= 1_000_000 {
        format!("{:.1} Mbps", bits_per_second as f64 / 1e6)
    } else {
        format!("{:.0} kbps", bits_per_second as f64 / 1e3)
    }
}

/// 照码率与时长估的大小：码率 × 时长 ÷ 8。
pub fn estimated_size(bits_per_second: u64, duration: Duration) -> u64 {
    (bits_per_second as f64 * duration.as_secs_f64() / 8.0) as u64
}

/// 路径：在主目录 `home` 下的写成 `~/…`。
pub fn path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_for_people() {
        assert_eq!(bytes(812), "812 B");
        assert_eq!(bytes(1_940_000_000), "1.9 GB");
        assert_eq!(bytes(999_949), "999.9 KB");
        assert_eq!(bytes(999_999), "1.0 MB");
        assert_eq!(clock(Duration::from_secs(48 * 60 + 12)), "48:12");
        assert_eq!(clock(Duration::from_secs(3600 + 12 * 60 + 34)), "1:12:34");
        assert_eq!(remaining(Duration::from_secs(42)), "剩余 42 秒");
        assert_eq!(remaining(Duration::from_secs(190)), "剩余 3 分 10 秒");
        assert_eq!(remaining(Duration::from_secs(3720)), "剩余 1 小时 2 分");
        assert_eq!(bitrate(6_300_000), "6.3 Mbps");
        assert_eq!(bitrate(128_000), "128 kbps");
        assert_eq!(
            estimated_size(8_000_000, Duration::from_secs(10)),
            10_000_000
        );
    }

    #[test]
    fn paths_under_home_are_shortened() {
        let home = Path::new("/Users/a");
        assert_eq!(path(Path::new("/Users/a/Movies"), Some(home)), "~/Movies");
        assert_eq!(path(Path::new("/Users/a"), Some(home)), "~");
        assert_eq!(path(Path::new("/Volumes/x"), Some(home)), "/Volumes/x");
        assert_eq!(path(Path::new("/Users/a/Movies"), None), "/Users/a/Movies");
    }
}
