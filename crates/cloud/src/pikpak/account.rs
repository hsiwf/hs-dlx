//! 登录后的账号：令牌与设备号，由调用方保存。

/// 一个登录了的 PikPak 账号。refresh_token 只能用一次：每次刷新都换成新的，旧的随即作废；[`super::Client`] 换了之后
/// 先交给调用方存下（见 [`super::Save`]），否则下次打开时只剩作废的那个，要重新登录。
#[derive(Clone, PartialEq, Eq)]
pub struct Account {
    /// 用户编号（令牌里的 `sub`）
    pub user_id: String,
    /// 设备号：32 位小写十六进制，登录时生成，之后的请求与 captcha 签名都用它
    pub device_id: String,
    pub refresh_token: String,
    pub access_token: String,
    /// access_token 到期的时刻，Unix 秒；比接口给的有效期早 2 分钟，免得请求在路上时到期
    pub access_expires_at: i64,
}

impl std::fmt::Debug for Account {
    /// 不写出令牌。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("user_id", &self.user_id)
            .field("device_id", &self.device_id)
            .field("access_expires_at", &self.access_expires_at)
            .finish_non_exhaustive()
    }
}
