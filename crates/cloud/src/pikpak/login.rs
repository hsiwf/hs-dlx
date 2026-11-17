//! 设备授权登录（RFC 8628）：向 PikPak 要一个登录码，用户在系统浏览器里打开官网的确认页登录并点确认（邮箱、手机号、
//! Google 都在官网上处理，不经手密码），这边按间隔轮询，确认了即拿到令牌。Google 不让在应用内嵌的网页里登录，所以
//! 确认页要在系统浏览器里打开，用网页版的 `/drive/qr-login`。

use std::time::Duration;

use hs_m3u8_core::{Engine, HttpOptions, Method, Url};

use super::client::{decode, identity_headers, now, send};
use super::profile::ANDROID;
use super::{Account, Endpoints, Error, wire};

/// 要的权限：网盘与离线下载。
const SCOPE: &str = "user pan offline";
/// 登录码没给有效期时按这么久算，秒。
const DEFAULT_EXPIRES_IN: i64 = 300;
/// 两次轮询至少隔这么久，秒（网盘给得更短时也按它）。
const MIN_INTERVAL: i64 = 2;

/// 一次进行中的登录。
#[derive(Debug, Clone)]
pub struct Login {
    device_code: String,
    /// 登录用的设备号，登录成了即是账号的设备号
    device_id: String,
    /// 确认页上显示的码，供用户核对
    pub user_code: String,
    /// 在系统浏览器里打开的确认页
    pub url: Url,
    /// 两次轮询至少隔多久；网盘要求放慢时变长
    pub interval: Duration,
    /// 登录码到期的时刻，Unix 秒
    pub expires_at: i64,
}

/// 轮询一次的结果。
#[derive(Debug)]
pub enum Poll {
    /// 还没确认
    Pending,
    /// 登录成了
    Done(Account),
}

/// 开始登录：生成设备号、要登录码。请求经 `http`（代理与重试；请求头与超时会被忽略，同 [`super::Client::new`]）。
pub async fn start(
    engine: &Engine,
    http: &HttpOptions,
    endpoints: &Endpoints,
) -> Result<Login, Error> {
    let device_id = new_device_id()?;
    let body = serde_json::json!({ "client_id": ANDROID.client_id, "scope": SCOPE });
    let url = endpoints.user("/v1/auth/device/code")?;
    let bytes = send(
        engine,
        http,
        Method::Post,
        url,
        identity_headers(&device_id),
        Some(&body),
    )
    .await?;
    let code: wire::DeviceCode = decode(&bytes)?;
    if code.device_code.is_empty() || code.user_code.is_empty() {
        return Err(Error::Unrecognized(
            "登录码的响应里缺 device_code 或 user_code".to_owned(),
        ));
    }
    let mut url = endpoints.web("/drive/qr-login")?;
    url.query_pairs_mut()
        .append_pair("client_id", ANDROID.client_id)
        .append_pair("scope", SCOPE)
        .append_pair("user_code", &code.user_code)
        .append_pair("platform", "hs-dlx");
    let interval = code.interval.unwrap_or(5).max(MIN_INTERVAL);
    Ok(Login {
        device_code: code.device_code,
        device_id,
        user_code: code.user_code,
        url,
        interval: Duration::from_secs(interval.unsigned_abs()),
        expires_at: now() + code.expires_in.unwrap_or(DEFAULT_EXPIRES_IN),
    })
}

impl Login {
    /// 问一次用户确认了没有。网盘要求放慢时把 [`Login::interval`] 加 5 秒、按还没确认算；登录码过期报
    /// [`Error::LoginExpired`]，用户拒绝报 [`Error::LoginDenied`]。
    pub async fn poll(
        &mut self,
        engine: &Engine,
        http: &HttpOptions,
        endpoints: &Endpoints,
    ) -> Result<Poll, Error> {
        if now() >= self.expires_at {
            return Err(Error::LoginExpired);
        }
        let body = serde_json::json!({
            "client_id": ANDROID.client_id,
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "device_code": self.device_code,
        });
        let url = endpoints.user("/v1/auth/token")?;
        let headers = identity_headers(&self.device_id);
        match send(engine, http, Method::Post, url, headers, Some(&body)).await {
            Ok(bytes) => {
                let token: wire::Token = decode(&bytes)?;
                Ok(Poll::Done(Account::from_token(
                    token,
                    self.device_id.clone(),
                )?))
            }
            Err(Error::Rejected(rejection)) => match rejection.name.as_str() {
                "authorization_pending" => Ok(Poll::Pending),
                _ if rejection.code == 4050 => Ok(Poll::Pending),
                "slow_down" => {
                    self.interval += Duration::from_secs(5);
                    Ok(Poll::Pending)
                }
                "expired_token" => Err(Error::LoginExpired),
                "access_denied" => Err(Error::LoginDenied),
                _ => Err(Error::Rejected(rejection)),
            },
            Err(error) => Err(error),
        }
    }
}

/// 新的设备号：16 个随机字节的小写十六进制。
fn new_device_id() -> Result<String, Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| Error::NoRandom(error.to_string()))?;
    Ok(crate::hex(&bytes))
}
