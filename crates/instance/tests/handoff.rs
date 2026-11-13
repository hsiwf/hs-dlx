//! 经真实的本机套接字转交：主实例收下链接与「到前台」，太长的链接两边都报错。

use std::path::PathBuf;

use hs_m3u8_instance::{Handoff, hand_over, listen};

/// 一份当作任务存储的文件：转交的地址由它的路径与属主定。
fn store(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("instance");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.sqlite"));
    std::fs::write(&path, b"").unwrap();
    path
}

#[test]
fn links_reach_the_running_instance() {
    let store = store("links");
    let mut received = listen(&store).unwrap();

    hand_over(&store, Some("https://a.example/x.m3u8")).unwrap();
    hand_over(&store, None).unwrap();

    assert_eq!(
        received.blocking_recv(),
        Some(Ok(Handoff::Link("https://a.example/x.m3u8".to_owned())))
    );
    assert_eq!(received.blocking_recv(), Some(Ok(Handoff::Activate)));
}

#[test]
fn another_store_has_its_own_endpoint() {
    let running = store("running");
    let _received = listen(&running).unwrap();
    // 别的存储没有主实例在收：连不上，明确报错
    let other = store("other");
    assert!(hand_over(&other, None).is_err());
}

#[test]
fn oversized_links_are_refused() {
    let store = store("oversized");
    let _received = listen(&store).unwrap();
    let long = "x".repeat(64 * 1024 + 1);
    assert!(hand_over(&store, Some(&long)).is_err());
}
