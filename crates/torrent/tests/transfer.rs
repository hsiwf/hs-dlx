//! 本机两个会话互传：一个做种，另一个用磁力链接（只有信息哈希）连上它，先取种子信息、开始之后再下完内容。不用 DHT、
//! tracker 与端口映射，不出本机。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hs_dlx_torrent::{AlertKind, Session, SessionConfig, State, TorrentId, create_torrent};

/// 测试用的空目录，名字带进程号。
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hs-dlx-torrent-{name}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 只在回环地址上监听（端口由系统挑）、不找外面的节点的会话。
fn local_session() -> Session {
    Session::new(&SessionConfig {
        listen_interfaces: "127.0.0.1:0".into(),
        dht: false,
        upnp: false,
        natpmp: false,
        lsd: false,
        user_agent: "hs-dlx-test".into(),
        peer_fingerprint: "-HD0000-".into(),
        state: Vec::new(),
    })
    .unwrap()
}

/// `len` 字节的内容，按 `seed` 变化：不同的文件内容不同。
fn content(len: u32, seed: u32) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_add(seed).wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect()
}

/// 做种：`path`（`dir` 里的文件或目录）建种子、在新会话里做种，等它可以连；返回会话、种子与种子文件。
fn seed(dir: &Path, path: &Path) -> (Session, TorrentId, Vec<u8>) {
    let torrent = create_torrent(path_str(path), 16 << 10).unwrap();
    let seeder = local_session();
    let id = seeder.add_torrent(&torrent, path_str(dir)).unwrap();
    seeder.start(&id).unwrap();
    wait_until(&seeder, Duration::from_secs(20), |s| {
        s.status(&id).unwrap().state() == Some(State::Seeding) && s.listen_port() != 0
    });
    (seeder, id, torrent)
}

/// 下完选了的文件（要下的字节都下完并校验过）。
fn done(session: &Session, id: &TorrentId) -> bool {
    let status = session.status(id).unwrap();
    status.has_metadata && status.pieces > 0 && status.total_wanted_done == status.total_wanted
}

/// 多文件的种子只下选了的文件；从续传数据恢复的会话不重下、也不连节点就是完成的。
#[test]
fn selected_files_download_and_resume_without_peers() {
    let seed_dir = scratch("multi-seed");
    let fetch_dir = scratch("multi-fetch");
    let album = seed_dir.join("album");
    std::fs::create_dir_all(album.join("extra")).unwrap();
    let files = [
        ("album/a.bin", content(100_000, 1)),
        ("album/b.bin", content(70_001, 2)),
        ("album/extra/c.bin", content(250_000, 3)),
    ];
    for (path, data) in &files {
        std::fs::write(seed_dir.join(path), data).unwrap();
    }
    let (seeder, id, torrent) = seed(&seed_dir, &album);

    let fetcher = local_session();
    assert_eq!(
        fetcher.add_torrent(&torrent, path_str(&fetch_dir)).unwrap(),
        id
    );
    // 已在会话里的不能再加：报错，另有一条加入失败的事件
    assert!(fetcher.add_torrent(&torrent, path_str(&fetch_dir)).is_err());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fetcher
        .pop_alerts(100)
        .unwrap()
        .iter()
        .any(|a| a.kind == AlertKind::AddFailed)
    {
        assert!(Instant::now() < deadline, "没等到加入失败的事件");
    }
    // 选好再开始
    let listed = fetcher.files(&id).unwrap();
    let mut paths: Vec<(String, i64)> = listed
        .iter()
        .map(|f| (f.path.replace('\\', "/"), f.size))
        .collect();
    paths.sort();
    let expected: Vec<(String, i64)> = files
        .iter()
        .map(|(path, data)| ((*path).to_owned(), i64::try_from(data.len()).unwrap()))
        .collect();
    assert_eq!(paths, expected);
    let wanted: Vec<i32> = listed
        .iter()
        .filter(|f| !f.path.ends_with("b.bin"))
        .map(|f| f.index)
        .collect();
    fetcher.select_files(&id, &wanted).unwrap();
    assert!(fetcher.select_files(&id, &[99]).is_err());
    fetcher.start(&id).unwrap();
    fetcher
        .connect_peer(&id, "127.0.0.1", seeder.listen_port())
        .unwrap();
    wait_until(&fetcher, Duration::from_secs(60), |s| done(s, &id));
    assert_eq!(fetcher.status(&id).unwrap().total_wanted, 100_000 + 250_000);
    // 写到磁盘后看文件；再写出续传数据，换一个不连任何节点的会话恢复
    flushed(&fetcher, &id);
    let resume = resume_data(&fetcher, &id);
    for (path, data) in [&files[0], &files[2]] {
        assert_file(&fetch_dir, path, data);
    }
    assert!(!fetch_dir.join("album/b.bin").exists());
    drop(fetcher);
    drop(seeder);
    let restored = local_session();
    let other = TorrentId::parse(&"0".repeat(40)).unwrap();
    assert!(
        restored
            .add_resume(&resume, &other, path_str(&fetch_dir))
            .is_err()
    );
    restored
        .add_resume(&resume, &id, path_str(&fetch_dir))
        .unwrap();
    wait_until(&restored, Duration::from_secs(20), |s| done(s, &id));
    assert_eq!(restored.status(&id).unwrap().peers, 0);
    // 移除完（移除的事件）之前不许再加同一个种子，之后可以
    restored.remove(&id, false).unwrap();
    assert!(restored.status(&id).is_err());
    assert!(
        restored
            .add_resume(&resume, &id, path_str(&fetch_dir))
            .is_err()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !restored
        .pop_alerts(100)
        .unwrap()
        .iter()
        .any(|a| a.kind == AlertKind::Removed && a.torrent == id.as_str())
    {
        assert!(Instant::now() < deadline, "没等到移除的事件");
    }
    restored
        .add_resume(&resume, &id, path_str(&fetch_dir))
        .unwrap();
    // 连文件一起删：libtorrent 先发删的结果、后发移除的事件，两个都到了才算移除完
    restored.remove(&id, true).unwrap();
    let (mut removed, mut deleted) = (false, false);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(removed && deleted) {
        assert!(Instant::now() < deadline, "没等到移除与删文件的事件");
        assert!(
            restored
                .add_resume(&resume, &id, path_str(&fetch_dir))
                .is_err(),
            "移除完之前加得进"
        );
        for alert in restored.pop_alerts(100).unwrap() {
            if alert.torrent != id.as_str() {
                continue;
            }
            match alert.kind {
                AlertKind::Removed => removed = true,
                AlertKind::Deleted | AlertKind::DeleteFailed => deleted = true,
                _ => {}
            }
        }
    }
    assert!(!fetch_dir.join("album/a.bin").exists(), "文件删掉了");
    restored
        .add_resume(&resume, &id, path_str(&fetch_dir))
        .unwrap();
    assert!(
        local_session()
            .add_resume(b"not bencode", &id, path_str(&fetch_dir))
            .is_err()
    );

    drop(restored);
    std::fs::remove_dir_all(&seed_dir).unwrap();
    std::fs::remove_dir_all(&fetch_dir).unwrap();
}

/// DHT 的状态写得出、下次建会话时载得进。
#[test]
fn session_state_round_trips() {
    let state = local_session().session_state().unwrap();
    assert!(!state.is_empty());
    let config = |state: Vec<u8>| SessionConfig {
        listen_interfaces: "127.0.0.1:0".into(),
        dht: false,
        upnp: false,
        natpmp: false,
        lsd: false,
        user_agent: "hs-dlx-test".into(),
        peer_fingerprint: "-HD0000-".into(),
        state,
    };
    Session::new(&config(state)).unwrap();
    assert!(Session::new(&config(b"not bencode".to_vec())).is_err());
}

/// 等到 `done` 成立，期间不断取走事件（不取时 libtorrent 的事件队列会满）；超过 `limit` 即失败。
fn wait_until(session: &Session, limit: Duration, mut done: impl FnMut(&Session) -> bool) {
    let deadline = Instant::now() + limit;
    while !done(session) {
        assert!(Instant::now() < deadline, "超过 {limit:?} 没等到");
        for alert in session.pop_alerts(100).unwrap() {
            assert!(
                !matches!(
                    alert.kind,
                    AlertKind::AddFailed | AlertKind::TorrentError | AlertKind::FileError
                ),
                "{alert:?}"
            );
        }
    }
}

/// 把 `id` 已下的数据写到磁盘并等它报来（libtorrent 先校验、再异步写盘，下完并校验过时数据可能还在内存里）。
fn flushed(session: &Session, id: &TorrentId) {
    session.flush(id).unwrap();
    wait_for(session, id, AlertKind::CacheFlushed, "写到磁盘");
}

/// 写出 `id` 的续传数据并等它报来，返回续传数据。
fn resume_data(session: &Session, id: &TorrentId) -> Vec<u8> {
    session.save_resume_data(id).unwrap();
    wait_for(session, id, AlertKind::ResumeData, "续传数据").data
}

/// 等到种子 `id` 的一条 `kind` 的事件（期间的别的事件丢掉）；20 秒内没等到即失败。
fn wait_for(
    session: &Session,
    id: &TorrentId,
    kind: AlertKind,
    what: &str,
) -> hs_dlx_torrent::Alert {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "没等到{what}");
        if let Some(alert) = session
            .pop_alerts(100)
            .unwrap()
            .into_iter()
            .find(|a| a.kind == kind && a.torrent == id.as_str())
        {
            return alert;
        }
    }
}

/// `dir` 下的 `relative`（`/` 分隔）是 `expected`：不在时列出 `dir` 下实际有的，不同时报两边的长度与第一个不同的位置
/// （整个内容打出来太长，CI 的日志里看不到）。
fn assert_file(dir: &Path, relative: &str, expected: &[u8]) {
    let path = relative
        .split('/')
        .fold(dir.to_path_buf(), |path, part| path.join(part));
    let actual = std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "读不出 {}（{error}）；{} 下有：{:?}",
            path.display(),
            dir.display(),
            tree(dir)
        )
    });
    if actual != expected {
        let first = actual.iter().zip(expected).position(|(a, b)| a != b);
        panic!(
            "{relative} 的内容不对：{} 字节，应为 {} 字节；第一个不同在 {first:?}",
            actual.len(),
            expected.len()
        );
    }
}

/// `dir` 下各文件的相对路径与字节数。
fn tree(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in std::fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::metadata(&path).unwrap();
            if meta.is_dir() {
                pending.push(path);
            } else {
                found.push((path.strip_prefix(dir).unwrap().to_path_buf(), meta.len()));
            }
        }
    }
    found.sort();
    found
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("临时目录的路径是 UTF-8")
}

#[test]
fn a_magnet_link_downloads_from_a_local_seed() {
    let seed_dir = scratch("seed");
    let fetch_dir = scratch("fetch");
    // 3 MiB 加零头的内容，分成 64 KiB 的块：最后一块不满
    let content: Vec<u8> = (0..(3 << 20) + 12_345)
        .map(|i: u32| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let file = seed_dir.join("sample.bin");
    std::fs::write(&file, &content).unwrap();
    let torrent = create_torrent(path_str(&file), 64 << 10).unwrap();

    let seeder = local_session();
    let seeding = seeder.add_torrent(&torrent, path_str(&seed_dir)).unwrap();
    seeder.start(&seeding).unwrap();
    wait_until(&seeder, Duration::from_secs(20), |s| {
        s.status(&seeding).unwrap().state() == Some(State::Seeding) && s.listen_port() != 0
    });

    // 名字不是 UTF-8（%FF）：照样加得进，名字里换成替换字符
    let fetcher = local_session();
    let magnet = format!("magnet:?xt=urn:btih:{seeding}&dn=a%FFb");
    let fetching: TorrentId = fetcher.add_magnet(&magnet, path_str(&fetch_dir)).unwrap();
    assert_eq!(fetching, seeding);
    assert_eq!(fetcher.status(&fetching).unwrap().name, "a\u{fffd}b");
    fetcher
        .connect_peer(&fetching, "127.0.0.1", seeder.listen_port())
        .unwrap();
    // 取到种子信息、校验完，还没开始：一块也不下（只上传的一方与做种的一方之后互相断开）
    wait_until(&fetcher, Duration::from_secs(60), |s| {
        let status = s.status(&fetching).unwrap();
        status.has_metadata && status.state() == Some(State::Downloading)
    });
    let held = fetcher.status(&fetching).unwrap();
    assert!(held.held);
    assert_eq!(held.total_wanted_done, 0);
    // 没下完的种子 libtorrent 不会自己发写完的事件：等得到即说明 flush 真的写了、事件转了出来
    flushed(&fetcher, &fetching);
    fetcher.start(&fetching).unwrap();
    assert!(!fetcher.status(&fetching).unwrap().held);
    fetcher
        .connect_peer(&fetching, "127.0.0.1", seeder.listen_port())
        .unwrap();
    wait_until(&fetcher, Duration::from_secs(60), |s| {
        let status = s.status(&fetching).unwrap();
        status.has_metadata && status.pieces > 0 && status.total_wanted_done == status.total_wanted
    });

    flushed(&fetcher, &fetching);
    let status = fetcher.status(&fetching).unwrap();
    assert_eq!(status.name, "sample.bin");
    assert_eq!(status.total_wanted, i64::try_from(content.len()).unwrap());
    assert_eq!(status.reachable_pieces, status.pieces);
    assert_file(&fetch_dir, "sample.bin", &content);

    drop(fetcher);
    drop(seeder);
    std::fs::remove_dir_all(&seed_dir).unwrap();
    std::fs::remove_dir_all(&fetch_dir).unwrap();
}
