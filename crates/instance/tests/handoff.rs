//! 经真实的本机套接字转交：界面接手后交出的一方才算交过去，不接手时报错；没有主实例、记录还在而主实例已不在的存储
//! 连不上；超长的链接两边都拒。

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hs_m3u8_instance::{Handoff, Handoffs, hand_over, listen, record_path};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, Stream};

/// 一份当作任务存储的文件：转交的地址记在它旁边。
fn store(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("instance");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.sqlite"));
    std::fs::write(&path, b"").unwrap();
    // 上一次运行留下的地址记录作废
    match std::fs::remove_file(record_path(&path)) {
        Ok(()) => {}
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
    path
}

/// 在另一个线程上交出 `link`；主实例这边收下这次转交，`accept` 时接手、否则释放它。返回交出的结果与接手的内容。
fn exchange(
    store: &Path,
    received: &mut Handoffs,
    link: Option<&str>,
    accept: bool,
) -> (Result<(), String>, Option<Handoff>) {
    let (store, link) = (store.to_owned(), link.map(str::to_owned));
    let giving = std::thread::spawn(move || hand_over(&store, link.as_deref()));
    let delivery = received.blocking_recv().unwrap().unwrap();
    let handoff = accept.then(|| delivery.accept());
    (giving.join().unwrap(), handoff)
}

#[test]
fn links_reach_the_running_instance_once_accepted() {
    let store = store("links");
    let mut received = listen(&store).unwrap();

    let (given, handoff) = exchange(
        &store,
        &mut received,
        Some("https://a.example/x.m3u8"),
        true,
    );
    given.unwrap();
    assert_eq!(
        handoff,
        Some(Handoff::Link("https://a.example/x.m3u8".to_owned()))
    );

    let (given, handoff) = exchange(&store, &mut received, None, true);
    given.unwrap();
    assert_eq!(handoff, Some(Handoff::Activate));

    // 界面不接手（如已在退出）：交出的一方报没交过去，由它告诉用户
    let (given, _) = exchange(&store, &mut received, Some("https://a.example/y"), false);
    assert!(given.is_err());

    // 空链接不是链接，交之前就拒
    assert!(hand_over(&store, Some("")).is_err());
}

#[test]
fn a_store_without_a_running_instance_cannot_be_reached() {
    // 没有记录：主实例还没开始收
    let other = store("other");
    assert!(hand_over(&other, None).is_err());

    // 记录还在而主实例已不在（异常退出，套接字没人收）：照记录连不上，很快报错
    let gone = store("gone");
    let missing = gone.with_file_name("gone.sock");
    std::fs::write(record_path(&gone), missing.to_str().unwrap()).unwrap();
    let started = Instant::now();
    assert!(hand_over(&gone, None).is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn oversized_links_are_refused_on_both_sides() {
    let store = store("oversized");
    let mut received = listen(&store).unwrap();
    let long = "x".repeat(64 * 1024 + 1);
    assert!(hand_over(&store, Some(&long)).is_err());

    // 绕过交出的一方的检查，直接写一个超长的长度：主实例报错、不回确认
    let endpoint = std::fs::read_to_string(record_path(&store)).unwrap();
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
