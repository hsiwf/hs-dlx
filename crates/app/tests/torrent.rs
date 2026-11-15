//! BT 任务的端到端测试：真实的 SQLite 文件，服务的 BT 会话只在回环地址上；做种的是测试自己建的另一个会话，磁力链接用
//! `x.pe` 给出它的地址（之后的任务从续传数据里记的节点连它）。服务自带 tokio 运行时，在测试自己的运行时外面建、外面
//! 丢弃；等事件在测试的运行时里。

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hs_dlx_app::{
    Draft, DraftKind, Error, Event, Service, Status, Store, TaskId, TorrentInfo, TorrentNetwork,
    TorrentProgress,
};
use hs_dlx_torrent::{AddOptions, Session, SessionConfig, State, TorrentId, create_torrent};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

/// 每个测试独立的空目录。
fn test_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("app-torrent")
        .join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `len` 字节的内容，按 `seed` 变化。
fn content(len: u32, seed: u32) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_add(seed).wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect()
}

const A: u32 = 100_000;
const B: u32 = 70_001;
/// 分块大小。libtorrent 建的是 v1 与 v2 兼有的种子，每个文件从新的一块开始（中间是对齐用的填充文件）：a.bin 占 7 块、
/// b.bin 占 5 块
const PIECE: i32 = 16 << 10;

/// 在 `dir` 下写 album/a.bin 与 album/b.bin，建种子；`without_b` 时建完再删掉 b.bin，做种的一方只有 a.bin 的块。
/// 在只监听回环地址的新会话里校验已有的文件后上传，返回会话与种子的编号。
fn seed(dir: &Path, without_b: bool) -> (Session, TorrentId) {
    let album = dir.join("album");
    std::fs::create_dir_all(&album).unwrap();
    std::fs::write(album.join("a.bin"), content(A, 1)).unwrap();
    std::fs::write(album.join("b.bin"), content(B, 2)).unwrap();
    let torrent = create_torrent(album.to_str().unwrap(), PIECE).unwrap();
    if without_b {
        std::fs::remove_file(album.join("b.bin")).unwrap();
    }
    let resume = hs_dlx_torrent::resume_from_torrent_file(&torrent).unwrap();
    let id = hs_dlx_torrent::describe(&resume).unwrap().id;
    let seeder = Session::new(&SessionConfig {
        listen_interfaces: "127.0.0.1:0".into(),
        dht: false,
        upnp: false,
        natpmp: false,
        lsd: false,
        user_agent: "hs-dlx-test-seeder".into(),
        peer_fingerprint: "-HD0000-".into(),
        state: Vec::new(),
    })
    .unwrap();
    let options = AddOptions {
        trust_pieces: false,
        paused: false,
    };
    seeder
        .add_resume(&resume, &id, dir.to_str().unwrap(), options)
        .unwrap();
    // 等它校验完、开始监听；取事件即是等待（不取时事件队列会满）
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state = seeder.status(&id).unwrap().state();
        if matches!(state, Some(State::Seeding | State::Downloading)) && seeder.listen_port() != 0 {
            return (seeder, id);
        }
        assert!(Instant::now() < deadline, "做种的一方 20 秒内没准备好");
        seeder.pop_alerts(100).unwrap();
    }
}

fn magnet(id: &TorrentId, seeder: &Session) -> String {
    format!(
        "magnet:?xt=urn:btih:{id}&x.pe=127.0.0.1:{}",
        seeder.listen_port()
    )
}

fn service(store: &Path) -> (Service, mpsc::UnboundedReceiver<Event>) {
    Service::new(
        Store::open(store).unwrap(),
        NonZeroUsize::new(8).unwrap(),
        TorrentNetwork::Loopback,
    )
    .unwrap()
}

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// 磁力链接取到的续传数据。
fn fetch(rt: &Runtime, service: &Service, uri: &str) -> Vec<u8> {
    let fetch = service.fetch_metadata(uri.to_owned()).unwrap();
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), fetch.resume)
            .await
            .expect("30 秒内没取到种子信息")
    })
    .unwrap()
    .unwrap()
}

fn draft(uri: &str, folder: &Path, resume: Vec<u8>, wanted: Vec<i32>) -> Draft {
    Draft {
        url: uri.parse().unwrap(),
        headers: Vec::new(),
        folder: folder.to_path_buf(),
        kind: DraftKind::Torrent { resume, wanted },
    }
}

/// 等到任务 `id` 停下、状态写进存储，返回它的状态；其间的进度交给 `on_progress`，它返回 true 时不再等、返回 None。
async fn until(
    events: &mut mpsc::UnboundedReceiver<Event>,
    id: TaskId,
    mut on_progress: impl FnMut(&TorrentProgress) -> bool,
) -> Option<Status> {
    let waiting = async {
        loop {
            match events.recv().await.expect("服务还在，事件通道不会关闭") {
                Event::TorrentProgress { id: at, progress } if at == id => {
                    if on_progress(&progress) {
                        return None;
                    }
                }
                Event::Ended {
                    id: at,
                    status,
                    unsaved,
                    ..
                } if at == id => {
                    assert!(unsaved.is_none(), "没写进存储：{unsaved:?}");
                    return Some(status);
                }
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(60), waiting)
        .await
        .expect("60 秒内没等到")
}

fn index_of(info: &TorrentInfo, name: &str) -> i32 {
    info.files
        .iter()
        .find(|file| file.path.ends_with(name))
        .unwrap()
        .index
}

/// 磁力链接取种子信息、只选一个文件下完：根名避开保存目录里已有的，完成的结果为选了的字节数；同一个种子不能再建，
/// 完成的可以从列表移除；关闭时记下 BT 会话的端口与 DHT 状态。
#[test]
fn a_magnet_link_downloads_the_chosen_file() {
    let dir = test_dir("download");
    let (seeder, id) = seed(&dir.join("seed"), false);
    let folder = dir.join("downloads");
    std::fs::create_dir_all(folder.join("album")).unwrap();
    let store = dir.join("tasks.sqlite");
    let rt = runtime();
    let (service, mut events) = service(&store);

    let uri = magnet(&id, &seeder);
    let resume = fetch(&rt, &service, &uri);
    let info = TorrentInfo::of(&resume).unwrap();
    assert_eq!(
        (info.name.as_str(), info.files.len(), info.done),
        ("album", 2, 0)
    );
    let wanted = vec![index_of(&info, "a.bin")];
    let task = rt
        .block_on(service.create(draft(&uri, &folder, resume.clone(), wanted.clone()), 1))
        .unwrap()
        .unwrap();
    assert_eq!(task.spec.title, "album 2");
    assert_eq!(task.spec.torrent().unwrap().root, folder.join("album 2"));
    assert_eq!(task.spec.torrent().unwrap().info_hash, id.as_str());

    let status = rt.block_on(until(&mut events, task.id, |_| false));
    assert_eq!(status, Some(Status::Done));
    let done = service.task(task.id).unwrap();
    assert_eq!(done.result.unwrap().bytes, u64::from(A));
    assert_eq!(
        std::fs::read(folder.join("album 2").join("a.bin")).unwrap(),
        content(A, 1)
    );
    assert!(!folder.join("album 2").join("b.bin").exists());

    let again = rt
        .block_on(service.create(draft(&uri, &folder, resume, wanted), 2))
        .unwrap();
    assert!(
        matches!(again, Err(Error::TorrentTaken(at)) if at == task.id),
        "{again:?}"
    );
    service.forget(task.id).unwrap();

    service.close().unwrap();
    drop(service);
    let conn = rusqlite::Connection::open(&store).unwrap();
    let (port, dht): (Option<i64>, Option<Vec<u8>>) = conn
        .query_row(
            "SELECT torrent_port, torrent_dht FROM settings WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(port.is_some() && dht.is_some_and(|dht| !dht.is_empty()));
    drop(seeder);
}

/// 做种的一方只有一部分：下到凑不齐为止（能凑齐的块数少于要下的）；暂停时存下已有的块，重开应用后看得到、继续时不
/// 重下；放弃时删掉下了的文件与记录。
#[test]
fn a_task_that_cannot_finish_keeps_what_it_has_until_discarded() {
    let dir = test_dir("partial");
    let (seeder, id) = seed(&dir.join("seed"), true);
    let folder = dir.join("downloads");
    let store = dir.join("tasks.sqlite");
    let rt = runtime();
    let (service, mut events) = service(&store);

    let uri = magnet(&id, &seeder);
    let resume = fetch(&rt, &service, &uri);
    let info = TorrentInfo::of(&resume).unwrap();
    let all = info.files.iter().map(|file| file.index).collect();
    let task = rt
        .block_on(service.create(draft(&uri, &folder, resume, all), 1))
        .unwrap()
        .unwrap();
    // a.bin 的 7 块，共 12 块
    let have = u64::from(A);
    let mut last = None;
    let stopped = rt.block_on(until(&mut events, task.id, |progress| {
        last = Some(progress.clone());
        progress.done == have && progress.reachable == 7
    }));
    assert_eq!(stopped, None, "凑不齐的任务停下了");
    let last = last.unwrap();
    assert_eq!(
        (last.pieces, last.wanted, last.seeds),
        (12, u64::from(A + B), 0)
    );

    service.pause(task.id);
    let status = rt.block_on(until(&mut events, task.id, |_| false));
    assert_eq!(status, Some(Status::Paused));
    let saved = rt
        .block_on(service.inspect_torrent(task.id))
        .unwrap()
        .unwrap();
    assert!(saved.trusted);
    assert_eq!(
        (saved.info.done, saved.info.wanted()),
        (have, u64::from(A + B))
    );

    service.close().unwrap();
    drop(service);
    let (service, mut events) = self::service(&store);
    assert_eq!(service.task(task.id).unwrap().status, Status::Paused);
    service.resume(task.id).unwrap();
    // 信续传数据：一开始就是已有的块，不重新校验
    let mut first = None;
    rt.block_on(until(&mut events, task.id, |progress| {
        first.get_or_insert_with(|| progress.clone());
        true
    }));
    assert_eq!(first.unwrap().done, have);

    let root = folder.join("album");
    assert!(root.join("a.bin").exists());
    let leftovers = rt.block_on(service.discard(task.id)).unwrap().unwrap();
    assert!(leftovers.is_empty());
    assert!(!root.exists(), "下了的文件与目录都删掉了");
    assert!(matches!(service.task(task.id), Err(Error::NotFound(_))));
    drop(service);
    drop(seeder);
}

/// 新建 BT 任务时核对选的文件与请求头；种子文件读不出时报错。
#[test]
fn torrent_drafts_are_checked() {
    let dir = test_dir("drafts");
    let (seeder, id) = seed(&dir.join("seed"), false);
    let rt = runtime();
    let (service, _events) = service(&dir.join("tasks.sqlite"));
    let uri = magnet(&id, &seeder);
    let resume = fetch(&rt, &service, &uri);
    let folder = dir.join("downloads");
    for (wanted, headers) in [
        (vec![], vec![]),
        (vec![99], vec![]),
        (vec![0], vec![("Cookie".to_owned(), "a=1".to_owned())]),
    ] {
        let mut draft = draft(&uri, &folder, resume.clone(), wanted);
        draft.headers = headers;
        let created = rt.block_on(service.create(draft, 1)).unwrap();
        assert!(
            matches!(created, Err(Error::InvalidInput(_))),
            "{created:?}"
        );
    }
    assert!(matches!(
        hs_dlx_app::torrent_from_file(b"not bencode"),
        Err(Error::InvalidInput(_))
    ));
    assert!(service.tasks().unwrap().is_empty());
    drop(service);
    drop(seeder);
}
