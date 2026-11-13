//! 经真实的本机套接字转交：主实例收下链接与「到前台」；没有主实例的存储连不上；超长的链接两边都拒。

use std::io::{Read as _, Write as _};
use std::path::PathBuf;

use hs_m3u8_instance::{Handoff, hand_over, listen};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, Stream};

/// 一份当作任务存储的文件：转交的地址记在它旁边。
fn store(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("instance");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.sqlite"));
    std::fs::write(&path, b"").unwrap();
    // 上一次运行留下的地址记录作废
    match std::fs::remove_file(dir.join(format!("{name}.sqlite.instance"))) {
        Ok(()) => {}
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
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
    // 空链接不是链接，交之前就拒
    assert!(hand_over(&store, Some("")).is_err());
}

#[test]
fn a_store_without_a_running_instance_cannot_be_reached() {
    let other = store("other");
    assert!(hand_over(&other, None).is_err());
}

#[test]
fn oversized_links_are_refused_on_both_sides() {
    let store = store("oversized");
    let mut received = listen(&store).unwrap();
    let long = "x".repeat(64 * 1024 + 1);
    assert!(hand_over(&store, Some(&long)).is_err());

    // 绕过交出的一方的检查，直接写一个超长的长度：主实例报错、不回确认
    let endpoint = std::fs::read_to_string(store.with_extension("sqlite.instance")).unwrap();
    let mut stream =
        Stream::connect(endpoint.as_str().to_fs_name::<GenericFilePath>().unwrap()).unwrap();
    stream
        .write_all(&(64 * 1024 + 1_u32).to_be_bytes())
        .unwrap();
    assert!(matches!(received.blocking_recv(), Some(Err(_))));
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    assert!(answer.is_empty());
}
