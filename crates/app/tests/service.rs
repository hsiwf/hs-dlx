//! 任务服务的端到端测试：真实的 SQLite 文件、测试内的本地 HLS 服务（分片取自 tests/fixtures/media/ts_a）。
//! 服务自带 tokio 运行时，在测试自己的运行时外面建、外面丢弃；等事件与本地服务在测试的运行时里。

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{StatusCode, Uri};
use hs_m3u8_app::{Draft, Error, Event, OutputKind, OutputSpec, Service, Status, Store, TaskId};
use hs_m3u8_core::Url;
use hs_m3u8_core::hls::Preference;
use tokio::runtime::Runtime;
use tokio::sync::{Notify, mpsc};

const PLAYLIST: &str =
    "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nseg0.ts\n#EXTINF:1,\nseg1.ts\n#EXT-X-ENDLIST\n";

/// 本地 HLS 服务：index.m3u8 与 ts_a 的两个分片；`gate` 打开前 seg1.ts 的请求挂着不回。
struct Server {
    base: Url,
    gate: Arc<Gate>,
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

#[derive(Clone)]
struct Files {
    bodies: Arc<HashMap<String, Vec<u8>>>,
    gate: Arc<Gate>,
}

async fn serve(State(files): State<Files>, uri: Uri) -> Result<Vec<u8>, StatusCode> {
    let path = uri.path().trim_start_matches('/');
    if path == "seg1.ts" {
        files.gate.pass().await;
    }
    files.bodies.get(path).cloned().ok_or(StatusCode::NOT_FOUND)
}

impl Server {
    async fn start(gate_open: bool) -> Server {
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/media/ts_a");
        let bodies = HashMap::from([
            ("index.m3u8".to_owned(), PLAYLIST.as_bytes().to_vec()),
            (
                "seg0.ts".to_owned(),
                std::fs::read(fixtures.join("seg0.ts")).unwrap(),
            ),
            (
                "seg1.ts".to_owned(),
                std::fs::read(fixtures.join("seg1.ts")).unwrap(),
            ),
        ]);
        let gate = Arc::new(Gate::default());
        if gate_open {
            gate.open();
        }
        let files = Files {
            bodies: Arc::new(bodies),
            gate: gate.clone(),
        };
        let app = axum::Router::new().fallback(serve).with_state(files);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Server { base, gate }
    }
}

/// 每个测试独立的空目录。
fn test_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("app")
        .join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn draft(url: Url, folder: PathBuf) -> Draft {
    Draft {
        url,
        preference: Preference::default(),
        headers: Vec::new(),
        output: OutputKind::Mp4,
        folder,
        live: false,
    }
}

/// 新建任务并等它存进存储。
fn create(rt: &Runtime, service: &Service, draft: Draft) -> hs_m3u8_app::Task {
    rt.block_on(service.create(draft, 1)).unwrap().unwrap()
}

fn service(store: &Path) -> (Service, mpsc::UnboundedReceiver<Event>) {
    Service::new(Store::open(store).unwrap(), NonZeroUsize::new(8).unwrap()).unwrap()
}

/// 等到任务 `id` 停下，返回它的状态与没写进存储的原因；中途的进度事件交给 `on_progress`。
async fn stopped(
    events: &mut mpsc::UnboundedReceiver<Event>,
    id: TaskId,
    mut on_progress: impl FnMut(&hs_m3u8_core::Progress),
) -> (Status, Option<Error>) {
    loop {
        match events.recv().await.expect("服务还在，事件通道不会关闭") {
            Event::Progress { id: at, progress } if at == id => on_progress(&progress),
            Event::Ended {
                id: at,
                status,
                unsaved,
                ..
            } if at == id => return (status, unsaved),
            _ => {}
        }
    }
}

/// 等到任务 `id` 停下、状态写进存储，返回它的状态；中途的进度事件交给 `on_progress`。
async fn ended(
    events: &mut mpsc::UnboundedReceiver<Event>,
    id: TaskId,
    on_progress: impl FnMut(&hs_m3u8_core::Progress),
) -> Status {
    match stopped(events, id, on_progress).await {
        (status, None) => status,
        (_, Some(error)) => panic!("没写进存储：{error}"),
    }
}

/// 等到任务 `id` 下完第一个分片。
async fn first_segment(events: &mut mpsc::UnboundedReceiver<Event>, id: TaskId) {
    loop {
        if let Some(Event::Progress { id: at, progress }) = events.recv().await
            && at == id
            && progress.segments.done == 1
        {
            return;
        }
    }
}

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// 新建即开始，下完写出 MP4（标题与主名取自地址）、删掉任务目录，存储里记为已完成；从列表移除只删记录，输出留着。
#[test]
fn a_task_downloads_and_is_recorded_as_done() {
    let dir = test_dir("done");
    let folder = dir.join("out");
    let rt = runtime();
    let server = rt.block_on(Server::start(true));
    let (service, mut events) = service(&dir.join("tasks.sqlite"));

    let task = create(
        &rt,
        &service,
        draft(server.base.join("index.m3u8").unwrap(), folder.clone()),
    );
    assert_eq!(task.spec.title, "index");
    assert_eq!(task.spec.output, OutputSpec::Mp4(folder.join("index.mp4")));
    let status = rt.block_on(ended(&mut events, task.id, |_| {}));

    assert_eq!(status, Status::Done);
    assert!(folder.join("index.mp4").is_file());
    assert!(!folder.join("index.hsdl").exists());
    let done = service.tasks().unwrap().remove(0);
    assert_eq!(done.status, Status::Done);
    let result = done.result.expect("完成时记下写出的结果");
    assert!(result.bytes > 0 && result.duration.is_some(), "{result:?}");
    assert_eq!(done.total, Some(2));
    assert!(matches!(
        service.resume(task.id),
        Err(Error::AlreadyDone(_))
    ));
    service.forget(task.id).unwrap();
    assert!(service.tasks().unwrap().is_empty());
    assert!(folder.join("index.mp4").is_file());
}

/// 第一个分片下完后暂停：停下为暂停、任务目录保留、总片数记下，查看任务目录得到已下的 1 片；关掉应用重开后改请求头、
/// 继续，只补下没完成的分片。
#[test]
fn a_paused_task_resumes_after_reopening() {
    let dir = test_dir("pause");
    let folder = dir.join("out");
    let store = dir.join("tasks.sqlite");
    let rt = runtime();
    let server = rt.block_on(Server::start(false));
    let id = {
        let (service, mut events) = service(&store);
        let id = create(
            &rt,
            &service,
            draft(server.base.join("index.m3u8").unwrap(), folder.clone()),
        )
        .id;
        let mut paused = false;
        let status = rt.block_on(ended(&mut events, id, |progress| {
            if progress.segments.done == 1 && !paused {
                service.pause(id);
                paused = true;
            }
        }));
        assert_eq!(status, Status::Paused);
        assert!(folder.join("index.hsdl").is_dir());
        id
    };

    server.gate.open();
    let (service, mut events) = service(&store);
    let task = service.tasks().unwrap().remove(0);
    assert_eq!((task.status, task.total), (Status::Paused, Some(2)));
    // 没完成的任务不能只从列表移除，任务目录要一起放弃
    assert!(matches!(service.forget(id), Err(Error::NotFinished(_))));
    let saved = rt.block_on(service.inspect(id)).unwrap().unwrap().unwrap();
    assert_eq!((saved.segments, saved.live), (1, false));
    // 停着时可以改请求头，不合要求的拒收
    let bad = vec![("Bad Name".to_owned(), "x".to_owned())];
    assert!(matches!(
        service.set_headers(id, bad),
        Err(Error::InvalidInput(_))
    ));
    let referer = vec![("Referer".to_owned(), "https://example.com/".to_owned())];
    service.set_headers(id, referer.clone()).unwrap();
    assert_eq!(service.tasks().unwrap()[0].spec.headers, referer);
    service.resume(id).unwrap();
    assert!(matches!(
        service.set_headers(id, Vec::new()),
        Err(Error::Running(_))
    ));
    let status = rt.block_on(ended(&mut events, id, |_| {}));

    assert_eq!(status, Status::Done);
    assert!(folder.join("index.mp4").is_file());
}

/// 同一地址新建两次：第二个任务的主名避开第一个任务占用的（第一个还没在磁盘上留下输出）。
#[test]
fn the_same_link_twice_gets_separate_outputs() {
    let dir = test_dir("twice");
    let folder = dir.join("out");
    let rt = runtime();
    let server = rt.block_on(Server::start(false));
    let (service, _events) = service(&dir.join("tasks.sqlite"));
    let url = server.base.join("index.m3u8").unwrap();

    let first = create(&rt, &service, draft(url.clone(), folder.clone()));
    let second = create(&rt, &service, draft(url, folder.clone()));

    assert_eq!(first.spec.output, OutputSpec::Mp4(folder.join("index.mp4")));
    assert_eq!(
        second.spec.output,
        OutputSpec::Mp4(folder.join("index 2.mp4"))
    );
    assert_eq!(second.spec.title, "index 2");
    for id in [first.id, second.id] {
        rt.block_on(service.discard(id)).unwrap().unwrap();
    }
}

/// 运行中放弃：先停下，再删掉任务目录与存储里的记录；放弃做完之前不能继续、再放弃或移除。
#[test]
fn discarding_a_running_task_removes_it() {
    let dir = test_dir("discard");
    let folder = dir.join("out");
    let rt = runtime();
    let server = rt.block_on(Server::start(false));
    let (service, mut events) = service(&dir.join("tasks.sqlite"));
    let id = create(
        &rt,
        &service,
        draft(server.base.join("index.m3u8").unwrap(), folder.clone()),
    )
    .id;
    rt.block_on(first_segment(&mut events, id));

    let discarding = service.discard(id);
    assert!(matches!(
        service.resume(id),
        Err(Error::Discarding(_) | Error::Running(_))
    ));
    assert!(matches!(
        service.forget(id),
        Err(Error::Discarding(_) | Error::Running(_))
    ));
    assert!(matches!(
        rt.block_on(service.discard(id)).unwrap(),
        Err(Error::Discarding(_))
    ));
    let leftovers = rt.block_on(discarding).unwrap().unwrap();

    assert_eq!(leftovers, []);
    assert!(!folder.join("index.hsdl").exists());
    assert!(service.tasks().unwrap().is_empty());
}

/// 保存目录不是绝对路径的拒收，什么也不存。
#[test]
fn a_relative_folder_is_refused() {
    let dir = test_dir("relative");
    let rt = runtime();
    let (service, _events) = service(&dir.join("tasks.sqlite"));
    let url = Url::parse("http://127.0.0.1:9/index.m3u8").unwrap();
    let refused = rt
        .block_on(service.create(draft(url, PathBuf::from("out")), 1))
        .unwrap();
    assert!(matches!(refused, Err(Error::InvalidInput(_))));
    assert!(service.tasks().unwrap().is_empty());
}

/// 任务停下时存储被别的连接占着：结束状态没写进去，事件里带着原因，服务记着它、列任务时按它返回；存储放开后关闭服务
/// 时补写进去，重开后是已完成。
#[test]
fn an_ending_that_cannot_be_saved_is_saved_later() {
    let dir = test_dir("unsaved");
    let folder = dir.join("out");
    let store = dir.join("tasks.sqlite");
    let rt = runtime();
    let server = rt.block_on(Server::start(false));
    let id = {
        let (service, mut events) = service(&store);
        let id = create(
            &rt,
            &service,
            draft(server.base.join("index.m3u8").unwrap(), folder.clone()),
        )
        .id;
        rt.block_on(first_segment(&mut events, id));
        let mut other = rusqlite::Connection::open(&store).unwrap();
        let holding = other
            .transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)
            .unwrap();
        server.gate.open();
        let (status, unsaved) = rt.block_on(stopped(&mut events, id, |_| {}));
        assert_eq!(status, Status::Done);
        assert!(matches!(unsaved, Some(Error::Store(_))), "{unsaved:?}");
        drop(holding);
        let task = service.tasks().unwrap().remove(0);
        assert_eq!(task.status, Status::Done);
        assert!(task.result.is_some());
        id
    };
    let (service, _events) = service(&store);
    let task = service.tasks().unwrap().remove(0);
    assert_eq!((task.id, task.status), (id, Status::Done));
    assert!(task.result.is_some());
}

/// 关闭服务：运行中的任务取消、停下、按暂停写进存储；之后不再开始任务。
#[test]
fn closing_pauses_running_tasks() {
    let dir = test_dir("close");
    let folder = dir.join("out");
    let rt = runtime();
    let server = rt.block_on(Server::start(false));
    let (service, mut events) = service(&dir.join("tasks.sqlite"));
    let url = server.base.join("index.m3u8").unwrap();
    let id = create(&rt, &service, draft(url.clone(), folder.clone())).id;
    rt.block_on(first_segment(&mut events, id));

    service.close().unwrap();

    let task = service.tasks().unwrap().remove(0);
    assert_eq!(task.status, Status::Paused);
    assert!(folder.join("index.hsdl").is_dir());
    assert!(matches!(service.resume(id), Err(Error::Closed)));
    let refused = rt.block_on(service.create(draft(url, folder), 1)).unwrap();
    assert!(matches!(refused, Err(Error::Closed)), "{refused:?}");
}

/// 新建时提交被别的连接挡住（它持着读事务）：报存储错误，已开始的任务取消、它建出的任务目录删掉，存储里没有它。
#[test]
fn a_create_that_cannot_commit_leaves_nothing() {
    let dir = test_dir("create-uncommitted");
    let folder = dir.join("out");
    let store = dir.join("tasks.sqlite");
    let rt = runtime();
    let server = rt.block_on(Server::start(false));
    let (service, _events) = service(&store);
    let mut other = rusqlite::Connection::open(&store).unwrap();
    let reading = other.transaction().unwrap();
    let count: i64 = reading
        .query_row("SELECT count(*) FROM task", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);

    let created = rt
        .block_on(service.create(
            draft(server.base.join("index.m3u8").unwrap(), folder.clone()),
            1,
        ))
        .unwrap();
    assert!(matches!(created, Err(Error::Store(_))), "{created:?}");
    drop(reading);
    // 关闭时等不要了的任务收尾（取消、删任务目录）
    service.close().unwrap();

    assert!(service.tasks().unwrap().is_empty());
    assert!(!folder.join("index.hsdl").exists());
}

/// 任务目录里有不是本库写的文件时放弃删不干净：任务留在列表里、报出留下的东西；拿走那个文件后再放弃即删掉。
#[test]
fn a_discard_that_leaves_the_work_dir_keeps_the_task() {
    let dir = test_dir("discard-left");
    let folder = dir.join("out");
    let rt = runtime();
    let server = rt.block_on(Server::start(false));
    let (service, mut events) = service(&dir.join("tasks.sqlite"));
    let id = create(
        &rt,
        &service,
        draft(server.base.join("index.m3u8").unwrap(), folder.clone()),
    )
    .id;
    rt.block_on(first_segment(&mut events, id));
    service.pause(id);
    assert_eq!(rt.block_on(ended(&mut events, id, |_| {})), Status::Paused);
    let foreign = folder.join("index.hsdl").join("notes.txt");
    std::fs::write(&foreign, b"mine").unwrap();

    let left = rt.block_on(service.discard(id)).unwrap();
    assert!(matches!(left, Err(Error::WorkDirLeft { .. })), "{left:?}");
    assert_eq!(service.tasks().unwrap().len(), 1);

    std::fs::remove_file(&foreign).unwrap();
    assert_eq!(rt.block_on(service.discard(id)).unwrap().unwrap(), []);
    assert!(service.tasks().unwrap().is_empty());
    assert!(!folder.join("index.hsdl").exists());
}
