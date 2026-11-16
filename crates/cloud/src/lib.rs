//! 云离线下载：替用户在网盘上离线下载磁力链接，再取回文件的直链。只管与网盘的接口，不知道任务、存储与界面；下载
//! 直链、校验文件由调用方做。

pub mod pikpak;

/// 字节转成小写十六进制。
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            write!(text, "{byte:02x}").expect("写进 String 不会失败");
            text
        })
}
