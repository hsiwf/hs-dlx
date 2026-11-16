//! 云离线任务的端到端测试：真实的 SQLite 文件、服务与核心的分段下载，对着测试内的本地模拟 PikPak（设备授权登录、离线
//! 任务、文件夹、带签名的直链；直链支持范围请求、带 ETag）。服务自带 tokio 运行时，在测试自己的运行时外面建、外面
//! 丢弃；等事件与本地服务在测试的运行时里。

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use hs_dlx_app::pikpak::gcid::Gcid;
use hs_dlx_app::{
    CloudProgress, CloudProvider, Draft, DraftKind, Error, Event, FailureKind, Kind, Service,
    Status, Store, TaskId,
};
use hs_m3u8_core::Url;
use serde_json::{Value, json};
use tokio::runtime::Runtime;
use tokio::sync::{Notify, mpsc};

const MAGNET: &str = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Show";

/// 网盘上的一个文件。
struct Remote {
    id: &'static str,
    name: &'static str,
    body: Vec<u8>,
    /// 网盘给的 GCID；None 时照内容算
    gcid: Option<&'static str>,
}

/// 模拟接口的状态。
struct Fake {
    /// 离线任务下到的：文件夹 Show（a.bin 与子文件夹 Sub/b.bin）
    files: Vec<Remote>,
    /// 各文件的直链被取了几次
    links: HashMap<String, u32>,
    /// 这些文件的直链第一次下载时回 403（过期）
    expire_once: Vec<&'static str>,
    /// 下载 b.bin 时挂着，直到打开
    hold_b: Arc<Gate>,
    /// 收到下载 b.bin 的请求时通知
    b_requested: Arc<Notify>,
    /// 提交离线下载的次数
    submits: u32,
    /// 提交时照常建任务，但响应体发到一半断开
    break_submit: bool,
}

#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    opened: Notify,
}

impl Gate {
    async fn pass(&self) {
        loop {
            let opened = self.opened.notified();
            if *self.open.lock().unwrap() {
                return;
            }
            opened.await;
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_waiters();
    }
}

type Shared = Arc<Mutex<Fake>>;

fn body(len: usize, seed: u32) -> Vec<u8> {
    (0..len as u32)
        .map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(seed) >> 13) as u8)
        .collect()
}

fn gcid(body: &[u8]) -> String {
    let mut gcid = Gcid::new(body.len() as u64);
    gcid.update(body);
    gcid.finish()
}

async fn device_code() -> Response {
    Json(json!({ "device_code": "dc", "user_code": "ABCD", "expires_in": 300, "interval": 1 }))
        .into_response()
}

async fn auth_token(Json(body): Json<Value>) -> Response {
    assert_ne!(body["grant_type"], "refresh_token", "测试里令牌不会过期");
    Json(json!({
        "access_token": "a1", "refresh_token": "r1", "expires_in": 7200, "sub": "user-1",
    }))
    .into_response()
}

async fn captcha_init() -> Response {
    Json(json!({ "captcha_token": "c1", "expires_in": 300 })).into_response()
}

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer a1")
}

fn task_body() -> Value {
    json!({
        "kind": "drive#task", "id": "VTask1", "name": "Show", "phase": "PHASE_TYPE_COMPLETE",
        "progress": 100, "file_id": "VShow", "file_size": "0", "params": { "url": MAGNET },
    })
}

async fn submit(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert!(authorized(&headers));
    assert_eq!(body["url"]["url"], MAGNET);
    let mut fake = fake.lock().unwrap();
    fake.submits += 1;
    if fake.break_submit {
        // 声明的长度比发出的长：发完这几个字节连接即断，客户端收到了响应头、没读完响应体
        return ([(header::CONTENT_LENGTH, "1000")], "{\"task\"").into_response();
    }
    Json(json!({ "task": task_body() })).into_response()
}

async fn tasks(headers: HeaderMap) -> Response {
    assert!(authorized(&headers));
    Json(json!({ "tasks": [task_body()] })).into_response()
}

fn file_json(fake: &Fake, remote: &Remote, base: &str) -> Value {
    let count = fake.links.get(remote.id).copied().unwrap_or(0);
    json!({
        "kind": "drive#file", "id": remote.id, "name": remote.name,
        "size": remote.body.len().to_string(),
        "hash": remote.gcid.map(str::to_owned).unwrap_or_else(|| gcid(&remote.body)).to_uppercase(),
        "web_content_link": format!("{base}dl/{}?n={count}", remote.id),
    })
}

fn base(headers: &HeaderMap) -> String {
    format!(
        "http://{}/",
        headers.get(header::HOST).unwrap().to_str().unwrap()
    )
}

async fn children(
    State(fake): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    assert!(authorized(&headers));
    let fake = fake.lock().unwrap();
    let base = base(&headers);
    let files = match query["parent_id"].as_str() {
        "VShow" => vec![
            file_json(&fake, &fake.files[0], &base),
            json!({ "kind": "drive#folder", "id": "VSub", "name": "Sub" }),
        ],
        "VSub" => vec![file_json(&fake, &fake.files[1], &base)],
        other => panic!("没有这个文件夹：{other}"),
    };
    Json(json!({ "files": files, "next_page_token": "" })).into_response()
}

async fn entry(
    State(fake): State<Shared>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Response {
    assert!(authorized(&headers));
    let mut fake = fake.lock().unwrap();
    if id == "VShow" {
        return Json(json!({ "kind": "drive#folder", "id": "VShow", "name": "Show" }))
            .into_response();
    }
    *fake.links.entry(id.clone()).or_default() += 1;
    let remote = fake.files.iter().find(|remote| remote.id == id).unwrap();
    Json(file_json(&fake, remote, &base(&headers))).into_response()
}

/// 直链：支持 `Range: bytes=a-` 与 `bytes=a-b`，带强 ETag；`expire_once` 里的第一个链接回 403。
async fn download(
    State(fake): State<Shared>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (data, hold) = {
        let fake = fake.lock().unwrap();
        if query["n"] == "1" && fake.expire_once.contains(&id.as_str()) {
            return StatusCode::FORBIDDEN.into_response();
        }
        let remote = fake.files.iter().find(|remote| remote.id == id).unwrap();
        if id == "VB" {
            fake.b_requested.notify_one();
        }
        (
            remote.body.clone(),
            (id == "VB").then(|| fake.hold_b.clone()),
        )
    };
    if let Some(gate) = hold {
        gate.pass().await;
    }
    let len = data.len() as u64;
    let etag = format!("\"{id}\"");
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("bytes="))
        .and_then(|value| value.split_once('-'))
        .map(|(start, end)| {
            let start: u64 = start.parse().unwrap();
            let end = if end.is_empty() {
                len - 1
            } else {
                end.parse::<u64>().unwrap().min(len - 1)
            };
            (start, end)
        });
    match range {
        Some((start, end)) => (
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}")),
                (header::ETAG, etag),
                (header::ACCEPT_RANGES, "bytes".to_owned()),
            ],
            data[start as usize..=end as usize].to_vec(),
        )
            .into_response(),
        None => ([(header::ETAG, etag)], data).into_response(),
    }
}

/// 起模拟接口，返回它的地址与状态。
async fn start(files: Vec<Remote>) -> (Url, Shared) {
    let fake = Shared::new(Mutex::new(Fake {
        files,
        links: HashMap::new(),
        expire_once: Vec::new(),
        hold_b: Arc::new(Gate::default()),
        b_requested: Arc::new(Notify::new()),
        submits: 0,
        break_submit: false,
    }));
    fake.lock().unwrap().hold_b.open();
    let app = axum::Router::new()
        .route("/v1/auth/device/code", post(device_code))
        .route("/v1/auth/token", post(auth_token))
        .route("/v1/shield/captcha/init", post(captcha_init))
        .route("/drive/v1/files", post(submit).get(children))
        .route("/drive/v1/files/{id}", get(entry))
        .route("/drive/v1/tasks", get(tasks))
        .route("/dl/{id}", get(download))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, fake)
}

fn remotes(b_gcid: Option<&'static str>) -> Vec<Remote> {
    vec![
        Remote {
            id: "VA",
            name: "a.bin",
            body: body(300_007, 1),
            gcid: None,
        },
        Remote {
            id: "VB",
            name: "b.bin",
            body: body(70_001, 2),
            gcid: b_gcid,
        },
    ]
}

fn test_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("cloud")
        .join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn service(dir: &Path, base: &Url) -> (Service, mpsc::UnboundedReceiver<Event>) {
    let service = Service::new(
        Store::open(&dir.join("tasks.sqlite")).unwrap(),
        NonZeroUsize::new(8).unwrap(),
        hs_dlx_app::TorrentNetwork::Loopback {
            public_trackers: Vec::new(),
        },
        hs_dlx_app::pikpak::Endpoints::at(base.clone()),
    )
    .unwrap();
    service.0.set_proxy(&hs_m3u8_core::Proxy::Direct).unwrap();
    service
}

/// 走一遍设备授权登录。
fn log_in(rt: &Runtime, service: &Service) {
    let login = rt.block_on(service.login_pikpak()).unwrap().unwrap();
    assert_eq!(login.user_code, "ABCD");
    rt.block_on(login.done).unwrap().unwrap();
    assert_eq!(service.pikpak_user().unwrap().as_deref(), Some("user-1"));
}

fn draft(folder: PathBuf) -> Draft {
    Draft {
        url: Url::parse(MAGNET).unwrap(),
        headers: Vec::new(),
        folder,
        kind: DraftKind::Cloud {
            provider: CloudProvider::PikPak,
        },
    }
}

/// 等到任务 `id` 停下、状态写进存储，返回它的状态；中途的云离线进度交给 `on_progress`。
async fn ended(
    events: &mut mpsc::UnboundedReceiver<Event>,
    id: TaskId,
    mut on_progress: impl FnMut(&CloudProgress),
) -> Status {
    loop {
        match events.recv().await.expect("服务还在，事件通道不会关闭") {
            Event::CloudProgress { id: at, progress } if at == id => on_progress(&progress),
            Event::Ended {
                id: at,
                status,
                unsaved,
                ..
            } if at == id => {
                assert!(unsaved.is_none(), "没写进存储：{unsaved:?}");
                return status;
            }
            _ => {}
        }
    }
}

/// 提交、等网盘下完、列出文件定下根（标题换成根名），逐个下载并校验；直链过期的重取一次。
#[test]
fn a_cloud_task_fetches_and_verifies_every_file() {
    let dir = test_dir("done");
    let folder = dir.join("out");
    let rt = runtime();
    let (base, fake) = rt.block_on(start(remotes(None)));
    fake.lock().unwrap().expire_once = vec!["VA"];
    let (service, mut events) = service(&dir, &base);
    log_in(&rt, &service);

    let task = rt
        .block_on(service.create(draft(folder.clone()), 1))
        .unwrap()
        .unwrap();
    assert_eq!(task.spec.title, "Show");
    let mut verified = false;
    let status = rt.block_on(ended(&mut events, task.id, |progress| {
        verified |= matches!(progress, CloudProgress::Verifying { .. });
    }));
    assert_eq!(status, Status::Done);
    assert!(verified);
    let root = folder.join("Show");
    let files = remotes(None);
    assert_eq!(std::fs::read(root.join("a.bin")).unwrap(), files[0].body);
    assert_eq!(
        std::fs::read(root.join("Sub").join("b.bin")).unwrap(),
        files[1].body
    );
    // 各文件的任务目录与它们的父目录都已删
    assert!(!folder.join("Show.hsdl").exists());
    let task = service.task(task.id).unwrap();
    let Kind::Cloud(spec) = &task.spec.kind else {
        panic!("还是云离线任务");
    };
    assert_eq!(spec.root.as_deref(), Some(root.as_path()));
    assert_eq!(task.result.unwrap().bytes, 370_008);
    // a.bin 的第一个直链过期，重取了一次
    assert_eq!(fake.lock().unwrap().links["VA"], 2);
}

/// 下完的文件与网盘给的 GCID 不符：删掉它，以校验不符失败；之前下完的照样记着。
#[test]
fn a_file_that_does_not_match_its_gcid_is_removed() {
    let dir = test_dir("mismatch");
    let folder = dir.join("out");
    let rt = runtime();
    let (base, _fake) = rt.block_on(start(remotes(Some(
        "0000000000000000000000000000000000000000",
    ))));
    let (service, mut events) = service(&dir, &base);
    log_in(&rt, &service);
    let task = rt
        .block_on(service.create(draft(folder.clone()), 1))
        .unwrap()
        .unwrap();
    match rt.block_on(ended(&mut events, task.id, |_| {})) {
        Status::Failed(failure) => assert_eq!(failure.kind, FailureKind::Integrity),
        other => panic!("应以校验不符失败：{other:?}"),
    }
    assert!(folder.join("Show").join("a.bin").exists());
    assert!(!folder.join("Show").join("Sub").join("b.bin").exists());
}

/// 下到一半暂停，再放弃：下了的文件、各文件的任务目录与内容的根都删掉，记录也删掉。
#[test]
fn discarding_a_paused_cloud_task_removes_what_it_fetched() {
    let dir = test_dir("discard");
    let folder = dir.join("out");
    let rt = runtime();
    let (base, fake) = rt.block_on(start(remotes(None)));
    let gate = Arc::new(Gate::default());
    fake.lock().unwrap().hold_b = gate.clone();
    let (service, mut events) = service(&dir, &base);
    log_in(&rt, &service);
    let task = rt
        .block_on(service.create(draft(folder.clone()), 1))
        .unwrap()
        .unwrap();
    // 等到在下第二个文件（b.bin 挂着）时暂停：挂着的请求收不到响应，进度要到读超时才变，所以等服务器收到请求
    let requested = fake.lock().unwrap().b_requested.clone();
    rt.block_on(requested.notified());
    service.pause(task.id);
    assert_eq!(
        rt.block_on(ended(&mut events, task.id, |_| {})),
        Status::Paused
    );
    assert!(folder.join("Show").join("a.bin").exists());
    rt.block_on(service.discard(task.id)).unwrap().unwrap();
    gate.open();
    assert!(!folder.join("Show").exists());
    assert!(!folder.join("Show.hsdl").exists());
    assert!(matches!(service.task(task.id), Err(Error::NotFound(_))));
}

/// 没登录时新建云离线任务报没登录，什么也不留。
#[test]
fn creating_a_cloud_task_needs_a_login() {
    let dir = test_dir("no-login");
    let rt = runtime();
    let (base, _fake) = rt.block_on(start(remotes(None)));
    let (service, _events) = service(&dir, &base);
    match rt
        .block_on(service.create(draft(dir.join("out")), 1))
        .unwrap()
    {
        Err(Error::NotLoggedIn) => {}
        other => panic!("应报没登录：{other:?}"),
    }
    assert!(service.tasks().unwrap().is_empty());
}

/// 提交时收到了响应头、没读完响应体（网盘已建了任务）：按链接在最近的离线任务里找回它，不重发，网盘上只建了一个。
#[test]
fn a_submit_cut_off_after_the_server_answered_is_found_not_resent() {
    let dir = test_dir("cut-off");
    let rt = runtime();
    let (base, fake) = rt.block_on(start(remotes(None)));
    fake.lock().unwrap().break_submit = true;
    let (service, mut events) = service(&dir, &base);
    log_in(&rt, &service);
    let task = rt
        .block_on(service.create(draft(dir.join("out")), 1))
        .unwrap()
        .unwrap();
    let Kind::Cloud(spec) = &task.spec.kind else {
        panic!("是云离线任务");
    };
    assert_eq!(spec.job, "VTask1");
    assert_eq!(fake.lock().unwrap().submits, 1);
    assert_eq!(
        rt.block_on(ended(&mut events, task.id, |_| {})),
        Status::Done
    );
}
