//! 登录后的接口调用：带上令牌、设备号与 captcha token，过期即换，换了的令牌先交给调用方存下再用。

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hs_m3u8_core::{Body, Call, Engine, HttpOptions, Method, Source, Timeouts, Url};
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

use super::profile::ANDROID;
use super::wire;
use super::{Account, Endpoints, Error, Reason, Rejection};

/// 响应体最多读多少字节：列目录一页 500 项约几百 KB。
const LIMIT: u64 = 8 << 20;
/// 接口请求的时限：都是小的 JSON，单次 30 秒（核心的默认 600 秒是为大分片定的），登录时等不了那么久。
const TIMEOUTS: Timeouts = Timeouts {
    connect: Duration::from_secs(10),
    read_idle: Duration::from_secs(20),
    request: Duration::from_secs(30),
};
/// access_token 在到期前这么多秒就换：避免请求在路上时到期。
const ACCESS_MARGIN: i64 = 120;

/// 存下换了的令牌：返回 Err 时为原因。
pub type Save = Box<dyn Fn(&Account) -> Result<(), String> + Send + Sync>;

/// 一个登录了的账号的状态：令牌与 captcha token，同一账号的各个 [`Client`] 共用。refresh_token 只能用一次，各自换
/// 令牌会互相作废，所以一个账号只能有一个。
pub struct Session {
    endpoints: Endpoints,
    state: Mutex<State>,
    save: Save,
}

impl Session {
    /// 以 `account` 调 `endpoints` 上的接口；换了令牌时先调 `save`，它报错时那次调用以 [`Error::Save`] 失败（换到的
    /// 令牌照样留在内存里用）。
    pub fn new(endpoints: Endpoints, account: Account, save: Save) -> Self {
        Session {
            endpoints,
            state: Mutex::new(State {
                account,
                captcha: String::new(),
            }),
            save,
        }
    }

    /// 现在的账号（含换过的令牌）。
    pub async fn account(&self) -> Account {
        self.state.lock().await.account.clone()
    }
}

/// 经一套请求设置调一个账号上的接口。
pub struct Client {
    engine: Engine,
    /// 代理、超时与重试；请求头由客户端加
    http: HttpOptions,
    session: Arc<Session>,
}

struct State {
    account: Account,
    /// 最近一次申请到的 captcha token；还没申请过为空。只在接口说它无效时重新申请
    captcha: String,
}

/// 发出请求时用的凭据，用于判断被拒之后别的请求是否已换过。
struct Sent {
    device_id: String,
    access: String,
    captcha: String,
}

impl Sent {
    fn of(state: &State) -> Sent {
        Sent {
            device_id: state.account.device_id.clone(),
            access: state.account.access_token.clone(),
            captcha: state.captcha.clone(),
        }
    }
}

impl Client {
    /// 调 `session` 的账号上的接口，请求经 `http`（代理与重试；其中的请求头与超时会被忽略：接口请求固定连接 10 秒、读空闲 20 秒、单次 30 秒）。取直链与下载它要走同一
    /// 出口：下载用同一份代理设置。
    pub fn new(engine: Engine, mut http: HttpOptions, session: Arc<Session>) -> Self {
        http.headers.clear();
        Client {
            engine,
            http,
            session,
        }
    }

    /// 发 `method` 请求到 `url`，带 JSON 请求体 `body`；`action` 为申请 captcha token 时报的动作（`<方法>:<路径>`）。
    /// access_token 被拒时换一次再发，captcha token 被拒时重新申请一次再发；响应体按 `T` 读出。
    pub(crate) async fn api<T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        body: Option<&serde_json::Value>,
        action: &str,
    ) -> Result<T, Error> {
        let (mut renewed_access, mut renewed_captcha) = (false, false);
        loop {
            let sent = self.ready().await?;
            let mut headers = identity_headers(&sent.device_id);
            headers.push((
                "Authorization".to_owned(),
                format!("Bearer {}", sent.access),
            ));
            if !sent.captcha.is_empty() {
                headers.push(("X-Captcha-Token".to_owned(), sent.captcha.clone()));
            }
            match send(&self.engine, &self.http, method, url.clone(), headers, body).await {
                Ok(bytes) => return decode(&bytes),
                Err(Error::Rejected(rejection)) => match rejection.reason() {
                    Reason::AccessExpired if !renewed_access => {
                        renewed_access = true;
                        self.refresh(&sent).await?;
                    }
                    Reason::Captcha if !renewed_captcha => {
                        renewed_captcha = true;
                        self.renew_captcha(action, &sent).await?;
                    }
                    _ => return Err(Error::Rejected(rejection)),
                },
                Err(error) => return Err(error),
            }
        }
    }

    /// 这次请求用的凭据：access_token 快到期时先换。
    async fn ready(&self) -> Result<Sent, Error> {
        let expired = {
            let state = self.session.state.lock().await;
            (state.account.access_expires_at <= now()).then(|| Sent::of(&state))
        };
        if let Some(sent) = expired {
            self.refresh(&sent).await?;
        }
        Ok(Sent::of(&*self.session.state.lock().await))
    }

    /// 用 refresh_token 换新令牌，先存下再用；`sent` 为被拒（或到期）的那次用的凭据，别的请求已换过时不再换。
    async fn refresh(&self, sent: &Sent) -> Result<(), Error> {
        let mut state = self.session.state.lock().await;
        if state.account.access_token != sent.access {
            return Ok(());
        }
        let body = serde_json::json!({
            "client_id": ANDROID.client_id,
            "client_secret": ANDROID.client_secret,
            "grant_type": "refresh_token",
            "refresh_token": state.account.refresh_token,
        });
        let url = self.session.endpoints.user("/v1/auth/token")?;
        let headers = identity_headers(&state.account.device_id);
        let bytes = send(
            &self.engine,
            &self.http,
            Method::Post,
            url,
            headers,
            Some(&body),
        )
        .await?;
        let token: wire::Token = decode(&bytes)?;
        let account = Account::from_token(token, state.account.device_id.clone())?;
        // 旧的 refresh_token 已作废：存不下也换上新的，这一次照样报错让调用方知道
        let saved = (self.session.save)(&account);
        state.account = account;
        saved.map_err(Error::Save)
    }

    /// 为动作 `action` 重新申请 captcha token；`sent` 为被拒的那次用的凭据，别的请求已换过时不再申请。要人机验证时报
    /// [`Error::Verification`]。
    async fn renew_captcha(&self, action: &str, sent: &Sent) -> Result<(), Error> {
        let mut state = self.session.state.lock().await;
        if state.captcha != sent.captcha {
            return Ok(());
        }
        let timestamp = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis()))
        .to_string();
        let device_id = state.account.device_id.clone();
        let body = serde_json::json!({
            "client_id": ANDROID.client_id,
            "action": action,
            "device_id": device_id,
            "captcha_token": state.captcha,
            "meta": {
                "captcha_sign": ANDROID.captcha_sign(&device_id, &timestamp),
                "client_version": ANDROID.client_version,
                "package_name": ANDROID.package_name,
                "user_id": state.account.user_id,
                "timestamp": timestamp,
            },
        });
        let mut url = self.session.endpoints.user("/v1/shield/captcha/init")?;
        url.query_pairs_mut()
            .append_pair("client_id", ANDROID.client_id);
        let bytes = send(
            &self.engine,
            &self.http,
            Method::Post,
            url,
            identity_headers(&device_id),
            Some(&body),
        )
        .await?;
        let captcha: wire::Captcha = decode(&bytes)?;
        if !captcha.url.is_empty() {
            return Err(Error::Verification(captcha.url));
        }
        if captcha.captcha_token.is_empty() {
            return Err(Error::Unrecognized(
                "申请 captcha token 的响应里没有 token".to_owned(),
            ));
        }
        state.captcha = captcha.captcha_token;
        Ok(())
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    pub(crate) fn endpoints(&self) -> &Endpoints {
        &self.session.endpoints
    }
}

/// 每个请求都带的客户端身份：User-Agent、设备号与 client_id。
pub(crate) fn identity_headers(device_id: &str) -> Vec<(String, String)> {
    vec![
        ("User-Agent".to_owned(), ANDROID.user_agent.to_owned()),
        ("X-Device-Id".to_owned(), device_id.to_owned()),
        ("X-Client-Id".to_owned(), ANDROID.client_id.to_owned()),
    ]
}

/// 发一次请求，返回 2xx 且没有错误体的响应体；非 2xx 或带错误体的报 [`Error::Rejected`]。
pub(crate) async fn send(
    engine: &Engine,
    http: &HttpOptions,
    method: Method,
    url: Url,
    headers: Vec<(String, String)>,
    body: Option<&serde_json::Value>,
) -> Result<Vec<u8>, Error> {
    let mut source = Source::new(url);
    source.http = http.clone();
    source.http.headers = headers;
    source.http.timeouts = TIMEOUTS;
    let call = Call {
        method,
        body: body.map(|body| Body {
            content_type: "application/json".to_owned(),
            data: serde_json::to_vec(body).expect("JSON 值总能写出"),
        }),
        limit: LIMIT,
    };
    let reply = engine
        .call(&source, &call)
        .await
        .map_err(Error::Transport)?;
    let envelope = serde_json::from_slice::<wire::ErrorBody>(&reply.body).ok();
    let failed = envelope
        .as_ref()
        .is_some_and(|body| body.error_code != 0 || !body.error.is_empty());
    if (200..300).contains(&reply.status) && !failed {
        return Ok(reply.body);
    }
    let envelope = envelope.unwrap_or(wire::ErrorBody {
        error: String::new(),
        error_code: 0,
        error_description: String::new(),
    });
    Err(Error::Rejected(Rejection {
        status: reply.status,
        name: envelope.error,
        code: envelope.error_code,
        description: envelope.error_description,
    }))
}

/// 按 `T` 读出响应体；认不出即接口变了。
pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(|error| {
        Error::Unrecognized(format!(
            "{}：{error}",
            std::any::type_name::<T>()
                .rsplit("::")
                .next()
                .unwrap_or("响应")
        ))
    })
}

/// 现在的 Unix 秒。
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

impl Account {
    /// 令牌响应 `token` 加上设备号成为账号；有效期留出 [`ACCESS_MARGIN`]。
    pub(crate) fn from_token(token: wire::Token, device_id: String) -> Result<Account, Error> {
        if token.access_token.is_empty() || token.refresh_token.is_empty() || token.sub.is_empty() {
            return Err(Error::Unrecognized(
                "令牌响应里缺 access_token、refresh_token 或 sub".to_owned(),
            ));
        }
        Ok(Account {
            user_id: token.sub,
            device_id,
            refresh_token: token.refresh_token,
            access_token: token.access_token,
            access_expires_at: now() + token.expires_in - ACCESS_MARGIN,
        })
    }
}
