//! PikPak 接口的错误：按调用方要怎么处理分类。

/// 接口回的错误（响应体里的 `error` 与 `error_code`，或没有错误体的非 2xx）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("PikPak 回了 HTTP {status}：{name}（{code}）{description}")]
pub struct Rejection {
    pub status: u16,
    /// 错误名；没有错误体时为空
    pub name: String,
    /// 错误码；没有错误体时为 0
    pub code: i64,
    pub description: String,
}

/// 错误的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// access_token 过期或失效：刷新令牌后重发一次
    AccessExpired,
    /// refresh_token 被拒（已用过、过期或账号退出了）：要用户重新登录
    LoginExpired,
    /// captcha token 无效或缺：重新申请后重发一次
    Captcha,
    /// 离线次数、空间或传输额度用尽：等额度恢复或升级会员
    Quota,
    /// 请求太频繁：过一阵再试
    RateLimited,
    /// 其余：照原样报出
    Other,
}

impl Rejection {
    /// 暂时的拒绝：网盘或途中的网关出错（5xx），或请求太频繁；过一阵再试可能就好。
    pub fn transient(&self) -> bool {
        self.status >= 500 || self.reason() == Reason::RateLimited
    }

    pub fn reason(&self) -> Reason {
        let name = self.name.as_str();
        // 9 也是「文件在回收站里」等错误的码，captcha 的错误名都以 captcha 开头
        if name.starts_with("captcha") {
            return Reason::Captcha;
        }
        if self.code == 4126 || name == "invalid_grant" {
            return Reason::LoginExpired;
        }
        if name == "unauthenticated" || matches!(self.code, 16 | 4121 | 4122) {
            return Reason::AccessExpired;
        }
        if matches!(self.code, 8 | 11)
            || name == "file_space_not_enough"
            || name.starts_with("task_daily_create_limit")
            || (name.ends_with("_exceeded") && name.contains("size"))
        {
            return Reason::Quota;
        }
        if self.code == 10 || self.status == 429 {
            return Reason::RateLimited;
        }
        if self.status == 401 && name.is_empty() {
            return Reason::AccessExpired;
        }
        Reason::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejection(status: u16, name: &str, code: i64) -> Rejection {
        Rejection {
            status,
            name: name.to_owned(),
            code,
            description: String::new(),
        }
    }

    #[test]
    fn reasons_follow_name_before_code() {
        assert_eq!(
            rejection(400, "captcha_invalid", 9).reason(),
            Reason::Captcha
        );
        // 同为 9，不是 captcha
        assert_eq!(
            rejection(400, "file_in_recycle_bin", 9).reason(),
            Reason::Other
        );
        assert_eq!(
            rejection(400, "invalid_grant", 4126).reason(),
            Reason::LoginExpired
        );
        assert_eq!(
            rejection(401, "unauthenticated", 16).reason(),
            Reason::AccessExpired
        );
        assert_eq!(rejection(401, "", 0).reason(), Reason::AccessExpired);
        assert_eq!(
            rejection(400, "task_daily_create_limit_vip", 11).reason(),
            Reason::Quota
        );
        assert_eq!(
            rejection(403, "monthly_cloud_download_size_exceeded", 0).reason(),
            Reason::Quota
        );
        assert_eq!(
            rejection(400, "file_space_not_enough", 8).reason(),
            Reason::Quota
        );
        assert_eq!(rejection(429, "", 0).reason(), Reason::RateLimited);
        assert_eq!(
            rejection(400, "file_duplicated_name", 3).reason(),
            Reason::Other
        );
    }
}
