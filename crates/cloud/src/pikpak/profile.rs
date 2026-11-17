//! 接口认的客户端身份：PikPak 安卓官方 App 的公开客户端参数。它们随 App 分发、任何人都能取得，不是本应用或用户的凭据；
//! 登录（设备授权）、刷新令牌与申请 captcha token 都按它，接口据此给出完整的 `/drive/v1` 权限与传输额度。

use md5::{Digest as _, Md5};

/// 一套客户端身份。版本号与签名盐是同一版 App 的，要一起换：官方 App 升级后旧的一套可能被拒，接口报 captcha 无效。
pub(crate) struct Profile {
    pub client_id: &'static str,
    pub client_secret: &'static str,
    pub user_agent: &'static str,
    pub package_name: &'static str,
    pub client_version: &'static str,
    /// 算 captcha 签名时依次接在后面做 md5 的串，次序有意义
    pub salts: &'static [&'static str],
}

/// 安卓 App 1.21.0：与 Piko（NihilDigit/pikpak-kotlin）用的一套。
pub(crate) const ANDROID: Profile = Profile {
    client_id: "YNxT9w7GMdWvEOKa",
    client_secret: "dbw2OtmVEeuUvIptb1Coyg",
    user_agent: "ANDROID-com.pikcloud.pikpak/1.21.0",
    package_name: "com.pikcloud.pikpak",
    client_version: "1.21.0",
    salts: &[
        "",
        "E32cSkYXC2bciKJGxRsE8ZgwmH/YwkvpD6/O9guSOa2irCwciH4xPHaH",
        "QtqgfMgHP2TFl",
        "zOKgHT56L7nIzFzDpUGhpWFrgP53m3G6ML",
        "S",
        "THxpsktzfFXizUv7DK1y/N7NZ1WhayViluBEvAJJ8bA1Wr6",
        "y9PXH3xGUhG/zQI8CaapRw2LhldCaFM9CRlKpZXJvj+pifu",
        "+RaaG7T8FRTI4cP019N5y9ofLyHE9ySFUr",
        "6Pf1l8UTeuzYldGtb/d",
    ],
};

impl Profile {
    /// 申请 captcha token 时 `meta.captcha_sign` 的值：client_id、版本、包名、设备号、时间戳（Unix 毫秒的十进制）拼起来，
    /// 依次接上每个盐做 md5（小写十六进制），前面加 `1.`。
    pub(crate) fn captcha_sign(&self, device_id: &str, timestamp_ms: &str) -> String {
        let mut hashed = format!(
            "{}{}{}{device_id}{timestamp_ms}",
            self.client_id, self.client_version, self.package_name
        );
        for salt in self.salts {
            hashed = crate::hex(&Md5::digest(format!("{hashed}{salt}")));
        }
        format!("1.{hashed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 期望值由 Python 的 hashlib 照同一算法算出。
    #[test]
    fn captcha_sign_chains_md5_over_salts() {
        assert_eq!(
            ANDROID.captcha_sign("0123456789abcdef0123456789abcdef", "1794819746396"),
            "1.f288ce210646d0d62db77fa36070ca68"
        );
    }
}
