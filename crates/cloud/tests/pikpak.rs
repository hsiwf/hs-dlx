//! PikPak 客户端对着本地的模拟接口：登录轮询、令牌过期换新、captcha 失效重申、离线任务、分页列目录、200 带错误体。
//! 模拟接口照 docs 里记下的 PikPak 行为：refresh_token 用过即作废；captcha token 按动作发，每个动作只认最近为它申请
//! 的那个。

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use hs_dlx_cloud::pikpak::login::{self, Poll};
use hs_dlx_cloud::pikpak::{Account, Client, Endpoints, Error, Phase, Reason, Session};
use hs_m3u8_core::{Engine, HttpOptions, Proxy, Url};
use serde_json::{Value, json};

/// 模拟接口的状态。
#[derive(Default)]
struct Fake {
    /// 设备授权已轮询过几次：第一次回「还没确认」
    polls: u32,
    /// 现在有效的令牌；换一次加一
    generation: u32,
    /// 各动作现在有效的 captcha token
    captcha: std::collections::HashMap<String, String>,
    /// 申请过几次 captcha token
    captcha_inits: u32,
    /// 下一个 drive 请求不论令牌都回 401（模拟服务端提前作废）
    revoke_once: bool,
    /// 提交离线时回 200 带错误体
    quota_exhausted: bool,
    /// 查离线任务时先回几次 503（网关出错）
    tasks_unavailable: u32,
}

type Shared = Arc<Mutex<Fake>>;

fn rejection(status: StatusCode, error: &str, code: i64) -> Response {
    (
        status,
        Json(json!({ "error": error, "error_code": code, "error_description": "" })),
    )
        .into_response()
}

fn token(generation: u32) -> Value {
    json!({
        "token_type": "Bearer",
        "access_token": format!("a{generation}"),
        "refresh_token": format!("r{generation}"),
        "expires_in": 7200,
        "sub": "user-1",
    })
}

async fn device_code(Json(body): Json<Value>) -> Response {
    assert_eq!(body["client_id"], "YNxT9w7GMdWvEOKa");
    Json(json!({ "device_code": "dc", "user_code": "ABCD-EFGH", "expires_in": 300, "interval": 1 }))
        .into_response()
}

async fn auth_token(State(fake): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut fake = fake.lock().unwrap();
    match body["grant_type"].as_str().unwrap() {
        "urn:ietf:params:oauth:grant-type:device_code" => {
            fake.polls += 1;
            if fake.polls == 1 {
                return rejection(StatusCode::BAD_REQUEST, "authorization_pending", 4050);
            }
            fake.generation = 1;
            Json(token(1)).into_response()
        }
        "refresh_token" => {
            if body["refresh_token"] != format!("r{}", fake.generation) {
                return rejection(StatusCode::BAD_REQUEST, "invalid_grant", 4126);
            }
            fake.generation += 1;
            Json(token(fake.generation)).into_response()
        }
        other => panic!("没有这种 grant_type：{other}"),
    }
}

async fn captcha_init(State(fake): State<Shared>, Json(body): Json<Value>) -> Response {
    assert!(
        body["meta"]["captcha_sign"]
            .as_str()
            .unwrap()
            .starts_with("1.")
    );
    assert_eq!(body["meta"]["user_id"], "user-1");
    let mut fake = fake.lock().unwrap();
    fake.captcha_inits += 1;
    let token = format!("c{}", fake.captcha_inits);
    let action = body["action"].as_str().unwrap().to_owned();
    fake.captcha.insert(action, token.clone());
    Json(json!({ "captcha_token": token, "expires_in": 300 })).into_response()
}

/// drive 接口（动作 `action`）的鉴权：令牌不是现在有效的回 401，captcha token 不是最近为这个动作申请的回 400；通过时
/// 为 None。
fn rejected(fake: &mut Fake, headers: &HeaderMap, action: &str) -> Option<Response> {
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if std::mem::take(&mut fake.revoke_once) || bearer != format!("Bearer a{}", fake.generation) {
        return Some(rejection(StatusCode::UNAUTHORIZED, "unauthenticated", 16));
    }
    assert_eq!(headers.get("x-device-id").unwrap().len(), 32);
    let captcha = headers
        .get("x-captcha-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if fake
        .captcha
        .get(action)
        .is_none_or(|token| token != captcha)
    {
        return Some(rejection(StatusCode::BAD_REQUEST, "captcha_invalid", 9));
    }
    None
}

fn task(phase: &str, progress: i64) -> Value {
    json!({
        "kind": "drive#task", "id": "VTask1", "name": "Big Buck Bunny", "type": "offline",
        "phase": phase, "progress": progress, "message": "", "file_id": "VFolder1",
        "file_size": "5368709120", "params": { "url": "magnet:?xt=urn:btih:abc" },
    })
}

async fn files_post(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let mut fake = fake.lock().unwrap();
    if let Some(response) = rejected(&mut fake, &headers, "POST:/drive/v1/files") {
        return response;
    }
    assert_eq!(body["upload_type"], "UPLOAD_TYPE_URL");
    assert_eq!(body["url"]["url"], "magnet:?xt=urn:btih:abc");
    if fake.quota_exhausted {
        // 有的接口 200 也带错误体
        return Json(json!({ "error": "task_daily_create_limit", "error_code": 11 }))
            .into_response();
    }
    Json(json!({ "upload_type": "UPLOAD_TYPE_URL", "task": task("PHASE_TYPE_RUNNING", 0) }))
        .into_response()
}

async fn tasks(State(fake): State<Shared>, headers: HeaderMap) -> Response {
    let mut fake = fake.lock().unwrap();
    if let Some(response) = rejected(&mut fake, &headers, "GET:/drive/v1/tasks") {
        return response;
    }
    if fake.tasks_unavailable > 0 {
        fake.tasks_unavailable -= 1;
        return (StatusCode::SERVICE_UNAVAILABLE, "<html>bad gateway</html>").into_response();
    }
    Json(json!({ "tasks": [task("PHASE_TYPE_COMPLETE", 100)], "next_page_token": "" }))
        .into_response()
}

fn file(id: &str, name: &str, size: &str) -> Value {
    json!({
        "kind": "drive#file", "id": id, "name": name, "size": size,
        "hash": "6805ED701E6EA96597858628F8999A955D441D9D", "phase": "PHASE_TYPE_COMPLETE",
    })
}

async fn files_get(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let mut fake = fake.lock().unwrap();
    if let Some(response) = rejected(&mut fake, &headers, "GET:/drive/v1/files") {
        return response;
    }
    assert_eq!(query["parent_id"], "VFolder1");
    match query["page_token"].as_str() {
        "" => Json(json!({
            "files": [{ "kind": "drive#folder", "id": "VSub", "name": "Extras" }, file("VA", "a.mkv", "262145")],
            "next_page_token": "p2",
        })),
        "p2" => Json(json!({ "files": [file("VB", "b.srt", "10")], "next_page_token": "" })),
        other => panic!("没有这一页：{other}"),
    }
    .into_response()
}

async fn file_get(
    State(fake): State<Shared>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let mut fake = fake.lock().unwrap();
    if let Some(response) = rejected(&mut fake, &headers, "GET:/drive/v1/files") {
        return response;
    }
    assert_eq!(query["usage"], "FETCH");
    let mut body = file(&id, "a.mkv", "262145");
    body["web_content_link"] = json!("https://dl.example/a.mkv?expire=1794819746&sign=x");
    Json(body).into_response()
}

async fn start() -> (Endpoints, Shared) {
    let fake = Shared::default();
    let app = axum::Router::new()
        .route("/v1/auth/device/code", post(device_code))
        .route("/v1/auth/token", post(auth_token))
        .route("/v1/shield/captcha/init", post(captcha_init))
        .route("/drive/v1/files", post(files_post).get(files_get))
        .route("/drive/v1/files/{id}", get(file_get))
        .route("/drive/v1/tasks", get(tasks))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Endpoints::at(base), fake)
}

fn http() -> HttpOptions {
    HttpOptions {
        proxy: Proxy::Direct,
        ..HttpOptions::default()
    }
}

fn engine() -> Engine {
    Engine::new(std::num::NonZeroUsize::new(4).unwrap())
}

/// 走一遍设备授权登录：第一次轮询还没确认，第二次拿到令牌。
async fn logged_in(endpoints: &Endpoints) -> Account {
    let mut login = login::start(&engine(), &http(), endpoints).await.unwrap();
    assert_eq!(login.user_code, "ABCD-EFGH");
    assert!(
        login
            .url
            .as_str()
            .contains("/drive/qr-login?client_id=YNxT9w7GMdWvEOKa")
    );
    assert!(login.url.as_str().contains("user_code=ABCD-EFGH"));
    // 网盘给的间隔不到下限时按下限
    assert_eq!(login.interval, std::time::Duration::from_secs(2));
    assert!(matches!(
        login.poll(&engine(), &http(), endpoints).await.unwrap(),
        Poll::Pending
    ));
    match login.poll(&engine(), &http(), endpoints).await.unwrap() {
        Poll::Done(account) => account,
        Poll::Pending => panic!("第二次轮询应已登录"),
    }
}

type Saved = Arc<Mutex<Vec<Account>>>;

fn client(endpoints: &Endpoints, account: Account) -> (Client, Saved) {
    let saved = Saved::default();
    let sink = saved.clone();
    let session = Session::new(
        endpoints.clone(),
        account,
        Box::new(move |account: &Account| {
            sink.lock().unwrap().push(account.clone());
            Ok(())
        }),
    );
    (Client::new(engine(), http(), Arc::new(session)), saved)
}

#[tokio::test]
async fn submits_and_follows_an_offline_download() {
    let (endpoints, fake) = start().await;
    let account = logged_in(&endpoints).await;
    assert_eq!(account.user_id, "user-1");
    assert_eq!(account.device_id.len(), 32);
    let (client, saved) = client(&endpoints, account);
    // 还没有 captcha token：第一次被拒，申请后重发
    let job = client.submit("magnet:?xt=urn:btih:abc").await.unwrap();
    assert_eq!(job.id, "VTask1");
    assert_eq!(job.phase, Phase::Running);
    assert_eq!(job.size, Some(5_368_709_120));
    assert_eq!(fake.lock().unwrap().captcha_inits, 1);
    let job = client.job("VTask1").await.unwrap().unwrap();
    assert_eq!((job.phase, job.progress), (Phase::Complete, 100));
    assert_eq!(job.file.as_deref(), Some("VFolder1"));
    assert_eq!(
        client
            .find_job("magnet:?xt=urn:btih:abc", &[])
            .await
            .unwrap()
            .map(|job| job.id),
        Some("VTask1".to_owned())
    );
    // 提交前已有的不算
    assert_eq!(
        client.job_ids_for("magnet:?xt=urn:btih:abc").await.unwrap(),
        ["VTask1"]
    );
    assert_eq!(
        client
            .find_job("magnet:?xt=urn:btih:abc", &["VTask1".to_owned()])
            .await
            .unwrap(),
        None
    );
    // 两页合起来；GCID 改成小写
    let children = client.children("VFolder1").await.unwrap();
    let names: Vec<&str> = children.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["Extras", "a.mkv", "b.srt"]);
    assert!(children[0].folder && children[0].gcid.is_none());
    assert_eq!(children[1].size, 262_145);
    assert_eq!(
        children[1].gcid.as_deref(),
        Some("6805ed701e6ea96597858628f8999a955d441d9d")
    );
    let entry = client.entry("VA").await.unwrap();
    assert_eq!(
        entry.link.unwrap().as_str(),
        "https://dl.example/a.mkv?expire=1794819746&sign=x"
    );
    // 一路没换过令牌；captcha token 每个动作申请一次（提交、查任务、列文件与取直链同为 GET /drive/v1/files），之后沿用
    assert!(saved.lock().unwrap().is_empty());
    assert_eq!(fake.lock().unwrap().captcha_inits, 3);
}

#[tokio::test]
async fn a_rejected_access_token_is_refreshed_and_saved_first() {
    let (endpoints, fake) = start().await;
    let account = logged_in(&endpoints).await;
    let (client, saved) = client(&endpoints, account);
    client.submit("magnet:?xt=urn:btih:abc").await.unwrap();
    fake.lock().unwrap().revoke_once = true;
    client.job("VTask1").await.unwrap().unwrap();
    let saved = saved.lock().unwrap().clone();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].access_token, "a2");
    assert_eq!(saved[0].refresh_token, "r2");
    assert_eq!(client.session().account().await, saved[0]);
}

#[tokio::test]
async fn an_expired_access_token_is_refreshed_before_sending() {
    let (endpoints, _fake) = start().await;
    let mut account = logged_in(&endpoints).await;
    account.access_expires_at = 0;
    let (client, saved) = client(&endpoints, account);
    client.submit("magnet:?xt=urn:btih:abc").await.unwrap();
    assert_eq!(saved.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_used_refresh_token_means_logging_in_again() {
    let (endpoints, fake) = start().await;
    let account = logged_in(&endpoints).await;
    let (first, _) = client(&endpoints, account.clone());
    fake.lock().unwrap().revoke_once = true;
    // 第一个客户端换掉了 r1
    first.submit("magnet:?xt=urn:btih:abc").await.unwrap();
    let (second, saved) = client(&endpoints, account);
    fake.lock().unwrap().revoke_once = true;
    match second.submit("magnet:?xt=urn:btih:abc").await {
        Err(Error::Rejected(rejection)) => assert_eq!(rejection.reason(), Reason::LoginExpired),
        other => panic!("应要求重新登录：{other:?}"),
    }
    assert!(saved.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_error_body_in_a_200_is_a_rejection() {
    let (endpoints, fake) = start().await;
    let account = logged_in(&endpoints).await;
    let (client, _) = client(&endpoints, account);
    fake.lock().unwrap().quota_exhausted = true;
    match client.submit("magnet:?xt=urn:btih:abc").await {
        Err(Error::Rejected(rejection)) => assert_eq!(rejection.reason(), Reason::Quota),
        other => panic!("应报额度用尽：{other:?}"),
    }
}

/// 同一账号的两个客户端（不同任务）共用一份令牌：一个换了，另一个照用新的，不会拿作废的 refresh_token 去换。
#[tokio::test]
async fn clients_of_one_session_share_refreshed_tokens() {
    let (endpoints, fake) = start().await;
    let account = logged_in(&endpoints).await;
    let (first, saved) = client(&endpoints, account);
    let second = Client::new(engine(), http(), first.session().clone());
    first.submit("magnet:?xt=urn:btih:abc").await.unwrap();
    fake.lock().unwrap().revoke_once = true;
    first.job("VTask1").await.unwrap().unwrap();
    fake.lock().unwrap().revoke_once = true;
    second.job("VTask1").await.unwrap().unwrap();
    // 第二次被拒时 second 换的是 r2（first 换到的），不是作废的 r1
    let saved = saved.lock().unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[1].refresh_token, "r3");
}

/// 查询被网关暂时拒绝（503）时退避后再试，不当成失败；作废的登录状态不再发请求。
#[tokio::test]
async fn queries_ride_out_a_gateway_error_and_a_retired_session_stops() {
    let (endpoints, fake) = start().await;
    let account = logged_in(&endpoints).await;
    let (client, _) = client(&endpoints, account);
    client.submit("magnet:?xt=urn:btih:abc").await.unwrap();
    fake.lock().unwrap().tasks_unavailable = 1;
    assert!(client.job("VTask1").await.unwrap().is_some());
    assert_eq!(fake.lock().unwrap().tasks_unavailable, 0);
    client.session().retire();
    assert!(matches!(client.job("VTask1").await, Err(Error::LoggedOut)));
}
