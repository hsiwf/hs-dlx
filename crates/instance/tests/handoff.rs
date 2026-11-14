//! 经真实的本机套接字转交：界面接手后交出的一方才算交过去；正在退出、晚接手时交出的一方报没交过去，晚接手的界面拿
//! 不到；没有主实例、记录还在而主实例已不在的存储连不上；超长的链接两边都拒。

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hs_dlx_instance::{Delivery, HandOverError, Handoff, Handoffs, hand_over, listen, record_path};
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

/// 在另一个线程上交出 `link`，主实例这边收下这次转交、交给 `answer` 答复，返回交出的结果与 `answer` 的结果。
fn exchange<T: Send + 'static>(
    store: &Path,
    received: &mut Handoffs,
    link: Option<&str>,
    answer: impl FnOnce(Delivery) -> T,
) -> (Result<(), HandOverError>, T) {
    let (store, link) = (store.to_owned(), link.map(str::to_owned));
    let giving = std::thread::spawn(move || hand_over(&store, link.as_deref()));
    let delivery = received.blocking_recv().unwrap().unwrap();
    let answered = answer(delivery);
    (giving.join().unwrap(), answered)
}

#[test]
fn links_reach_the_running_instance_once_accepted() {
    let store = store("links");
    let mut received = listen(&store).unwrap();

    let link = Some("https://a.example/x.m3u8");
    let (given, handoff) = exchange(&store, &mut received, link, Delivery::accept);
    given.unwrap();
    assert_eq!(
        handoff,
        Some(Handoff::Link("https://a.example/x.m3u8".to_owned()))
    );

    let (given, handoff) = exchange(&store, &mut received, None, Delivery::accept);
    given.unwrap();
    assert_eq!(handoff, Some(Handoff::Activate));

    // 空链接不是链接，交之前就拒
    assert!(hand_over(&store, Some("")).is_err());
}

#[test]
fn an_instance_that_does_not_take_the_link_says_so() {
    let store = store("refused");
    let mut received = listen(&store).unwrap();
    let link = Some("https://a.example/y");

    // 正在退出：交出的一方知道是这个原因
    let (given, ()) = exchange(&store, &mut received, link, Delivery::refuse_leaving);
    assert_eq!(given, Err(HandOverError::Leaving));

    // 界面 2 秒后才接手：交出的一方已报没交过去，界面拿不到这次转交，两边说法一致
    let (given, late) = exchange(&store, &mut received, link, |delivery| delivery);
    assert!(matches!(given, Err(HandOverError::Failed(_))));
    assert_eq!(late.accept(), None);
}

#[test]
fn a_store_without_a_running_instance_cannot_be_reached() {
    // 没有记录：主实例还没开始收
    let other = store("other");
    assert!(hand_over(&other, None).is_err());

    // 记录还在而主实例已不在（异常退出）：Unix 上套接字文件还在、没人收，Windows 上管道已不在；照记录连不上，很快报错
    let gone = store("gone");
    #[cfg(unix)]
    let missing = {
        let path = gone.with_file_name("gone.sock");
        // 上一次运行留下的先删掉；建出套接字文件后关掉监听，留下没人收的套接字
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        }
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        path
    };
    #[cfg(windows)]
    let missing = PathBuf::from(r"\\.\pipe\hs-dlx-test-gone");
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
