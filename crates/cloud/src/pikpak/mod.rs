//! PikPak（mypikpak.com）的非官方接口：以安卓官方 App 的身份（公开的客户端参数，写在 profile 模块里）登录用户自己的账号（[`login`]）、提交
//! 离线下载、查进度、列文件、取直链（[`Client`]）。接口与错误码没有公开文档，依据其他客户端的实现与实测，随时可能变。

mod account;
mod client;
mod drive;
mod error;
pub mod gcid;
pub mod login;
mod profile;
mod wire;

pub use account::Account;
pub use client::{Client, Save, Session};
pub use drive::{Entry, Job, Phase};
pub use error::{Reason, Rejection};

use hs_m3u8_core::Url;

/// 调 PikPak 接口的失败。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 没拿到完整的响应（连不上、超时、代理、证书等），按核心的分类
    #[error("PikPak：{0}")]
    Transport(hs_m3u8_core::Error),
    /// 接口拒绝了（见 [`Rejection::reason`]）
    #[error(transparent)]
    Rejected(Rejection),
    /// 响应认不出：接口变了
    #[error("PikPak 回的内容认不出（接口可能变了）：{0}")]
    Unrecognized(String),
    /// 要用户在浏览器里打开这个地址过人机验证
    #[error("PikPak 要求人机验证")]
    Verification(String),
    /// 换了的令牌存不下：令牌照样在用，下次打开应用时存着的已作废、要重新登录
    #[error("PikPak 换了的登录令牌存不下：{0}")]
    Save(String),
    /// 登录没等到确认就过期了
    #[error("登录码已过期，重新登录")]
    LoginExpired,
    /// 用户在浏览器里拒绝了登录
    #[error("在浏览器里拒绝了登录")]
    LoginDenied,
    /// 已退出登录或换了账号（[`Session::retire`]），这个账号的请求不再发
    #[error("已退出 PikPak 登录或换了账号")]
    LoggedOut,
    /// 本机取不到随机数，生成不了设备号
    #[error("本机取不到随机数：{0}")]
    NoRandom(String),
}

/// 接口所在的地址：官方的，或测试用的本地服务器。
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// 登录、令牌、captcha
    user: Url,
    /// 网盘与离线下载
    drive: Url,
    /// 网页版：用户在浏览器里确认登录的页面
    web: Url,
}

impl Endpoints {
    pub fn official() -> Endpoints {
        let parse = |text: &str| Url::parse(text).expect("写死的地址合法");
        Endpoints {
            user: parse("https://user.mypikpak.com"),
            drive: parse("https://api-drive.mypikpak.com"),
            web: parse("https://mypikpak.com"),
        }
    }

    /// 三者都在 `base` 上（测试用）。
    pub fn at(base: Url) -> Endpoints {
        Endpoints {
            user: base.clone(),
            drive: base.clone(),
            web: base,
        }
    }

    pub(crate) fn user(&self, path: &str) -> Result<Url, Error> {
        join(&self.user, path)
    }

    pub(crate) fn drive(&self, path: &str) -> Result<Url, Error> {
        join(&self.drive, path)
    }

    pub(crate) fn web(&self, path: &str) -> Result<Url, Error> {
        join(&self.web, path)
    }
}

/// `base` 上的路径 `path`（以 `/` 开头）。路径来自接口给的编号时可能拼不出合法地址，算接口变了。
fn join(base: &Url, path: &str) -> Result<Url, Error> {
    base.join(path)
        .map_err(|error| Error::Unrecognized(format!("拼不出地址 {path}：{error}")))
}
