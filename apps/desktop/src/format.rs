//! 界面上数字与路径的写法。

use std::path::Path;
use std::time::Duration;

use hs_m3u8_app::FailureKind;
use hs_m3u8_core::VariantInfo;

/// 在运行的任务数与合计速度（「2 个进行中 · ↓ 18.4 MB/s」）；没有在运行的任务时为 None。
pub fn activity(activity: &crate::tasks::Activity) -> Option<String> {
    (activity.running > 0).then(|| {
        format!(
            "{} 个进行中 · ↓ {}/s",
            activity.running,
            bytes(activity.speed as u64)
        )
    })
}

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

/// 剩余时间：向上取整到秒，只写最大的两级（`剩余 42 秒`、`剩余 3 分 10 秒`、`剩余 1 小时 2 分`）。
pub fn remaining(duration: Duration) -> String {
    let total = duration.as_secs() + u64::from(duration.subsec_nanos() > 0);
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("剩余 {h} 小时 {m} 分")
    } else if m > 0 {
        format!("剩余 {m} 分 {s} 秒")
    } else {
        format!("剩余 {s} 秒")
    }
}

/// 码率：bit/s 写成 Mbps（一位小数）或 kbps（整数）；四舍五入后到 1000 kbps 的写成 Mbps。
pub fn bitrate(bits_per_second: u64) -> String {
    let kbps = (bits_per_second as f64 / 1e3).round();
    if kbps >= 1000.0 {
        format!("{:.1} Mbps", bits_per_second as f64 / 1e6)
    } else {
        format!("{kbps:.0} kbps")
    }
}

/// 照变体的码率（BANDWIDTH，播放列表里写的峰值）与时长估的大小的上限：`至多约 1.9 GB`。
pub fn size_bound(bits_per_second: u64, duration: Duration) -> String {
    let size = (bits_per_second as f64 * duration.as_secs_f64() / 8.0) as u64;
    format!("至多约 {}", bytes(size))
}

/// 一档清晰度的简称：有分辨率时为高度（如 1080p），否则为码率，都没有时为它在主播放列表里的序号。
pub fn variant(v: &VariantInfo) -> String {
    match (v.resolution, v.bandwidth) {
        (Some(r), _) => format!("{}p", r.height),
        (None, Some(bw)) => bitrate(bw),
        (None, None) => format!("第 {} 档", v.index + 1),
    }
}

/// 路径：在主目录 `home` 下的写成 `~/…`。
pub fn path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// 失败在表盘上的大字与说明：大字为状态码或种类，说明写这类失败多半是怎么回事（完整的错误另外列出）。
pub fn failure(kind: FailureKind) -> (String, &'static str) {
    match kind {
        FailureKind::Status(code @ (401 | 403)) => (
            code.to_string(),
            "服务器拒绝了请求，多要带上来源页的 Referer 或 Cookie",
        ),
        FailureKind::Status(code @ (404 | 410)) => {
            (code.to_string(), "服务器上已没有这个地址，多为链接过期")
        }
        FailureKind::Status(code) => (code.to_string(), "服务器没有正常回应"),
        FailureKind::Network => (
            "网络".into(),
            "连不上服务器或传输中断，检查网络或代理后重试",
        ),
        FailureKind::Certificate => ("证书".into(), "服务器的证书不受信任"),
        FailureKind::Disk => ("磁盘".into(), "读写本机的文件出错，或读不到所在的盘"),
        FailureKind::OutputTaken => ("已存在".into(), "保存位置上已有同名的文件或目录"),
        FailureKind::SourceChanged => (
            "不符".into(),
            "来源与已下载的接不上：点播只能重新下载，直播可以写出已录的",
        ),
        FailureKind::Malformed => (
            "内容".into(),
            "取到的不是视频该有的内容，多为错误页或登录页，常要补 Cookie",
        ),
        FailureKind::Stalled => ("停滞".into(), "直播长时间录不到新内容"),
        FailureKind::Mp4Unsupported => (
            "不兼容".into(),
            "内容放不进 MP4：改存本地 HLS，已下的照样用",
        ),
        FailureKind::NothingToWrite => {
            ("空".into(), "没有可写出的已录内容：接着下载或录制后再写出")
        }
        FailureKind::Unsupported => ("不支持".into(), "来源用了不支持的特性，或不是 HLS"),
        FailureKind::Unrecoverable => ("!".into(), "再试也一样，原因见错误信息"),
        FailureKind::Other => ("!".into(), "原因见错误信息"),
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
        assert_eq!(remaining(Duration::from_millis(600)), "剩余 1 秒");
        assert_eq!(bitrate(6_300_000), "6.3 Mbps");
        assert_eq!(bitrate(128_000), "128 kbps");
        assert_eq!(bitrate(999_499), "999 kbps");
        assert_eq!(bitrate(999_999), "1.0 Mbps");
        assert_eq!(
            size_bound(8_000_000, Duration::from_secs(10)),
            "至多约 10.0 MB"
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
