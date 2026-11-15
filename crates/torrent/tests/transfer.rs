//! 本机两个会话互传：一个做种，另一个用磁力链接（只有信息哈希）连上它，先取种子信息再下完内容。不用 DHT、tracker 与
//! 端口映射，不出本机。

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
    })
    .unwrap()
}

/// 等到 `done` 成立，期间不断取走事件（不取时 libtorrent 的事件队列会满）；超过 `limit` 即失败。
fn wait_until(session: &Session, limit: Duration, mut done: impl FnMut(&Session) -> bool) {
    let deadline = Instant::now() + limit;
    while !done(session) {
        assert!(Instant::now() < deadline, "超过 {limit:?} 没等到");
        for alert in session.pop_alerts(100) {
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
    wait_until(&seeder, Duration::from_secs(20), |s| {
        s.status(&seeding).unwrap().state() == Some(State::Seeding) && s.listen_port() != 0
    });

    let fetcher = local_session();
    let magnet = format!("magnet:?xt=urn:btih:{seeding}");
    let fetching: TorrentId = fetcher.add_magnet(&magnet, path_str(&fetch_dir)).unwrap();
    assert_eq!(fetching, seeding);
    fetcher
        .connect_peer(&fetching, "127.0.0.1", seeder.listen_port())
        .unwrap();
    wait_until(&fetcher, Duration::from_secs(60), |s| {
        let status = s.status(&fetching).unwrap();
        status.has_metadata && status.pieces > 0 && status.total_wanted_done == status.total_wanted
    });

    let status = fetcher.status(&fetching).unwrap();
    assert_eq!(status.name, "sample.bin");
    assert_eq!(status.total_wanted, i64::try_from(content.len()).unwrap());
    assert_eq!(status.reachable_pieces, status.pieces);
    assert_eq!(
        std::fs::read(fetch_dir.join("sample.bin")).unwrap(),
        content
    );

    drop(fetcher);
    drop(seeder);
    std::fs::remove_dir_all(&seed_dir).unwrap();
    std::fs::remove_dir_all(&fetch_dir).unwrap();
}
