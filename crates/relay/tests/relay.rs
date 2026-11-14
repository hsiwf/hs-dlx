//! 经真实的本机连接：中转收 CONNECT 与写着完整地址的 http 请求，直连目标，或经要口令的上游 HTTP 代理与 SOCKS5
//! 代理连过去；路线给不出、连不上时回 502 并报给调用方；释放即停。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use hs_m3u8_relay::{Relay, Report, Route, Target, Upstream};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

/// 把收到的字节原样送回的服务。
async fn echo() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut reader, mut writer) = stream.split();
                tokio::io::copy(&mut reader, &mut writer).await.unwrap();
            });
        }
    });
    address
}

/// 读到空行为止的请求头各行。
async fn read_head(stream: &mut BufReader<TcpStream>) -> Vec<String> {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        if line == "\r\n" || line.is_empty() {
            return lines;
        }
        lines.push(line.trim_end().to_owned());
    }
}

/// 回一个 http 响应、记下请求头的服务。
async fn origin(seen: Arc<Mutex<Vec<String>>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut stream = BufReader::new(stream);
                let head = read_head(&mut stream).await;
                seen.lock().unwrap().extend(head);
                stream
                    .get_mut()
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\norigin")
                    .await
                    .unwrap();
            });
        }
    });
    address
}

/// 要 Basic 口令的上游 HTTP 代理：CONNECT 连到请求的地址，完整地址的请求由它自己回。记下请求头。
async fn http_proxy(seen: Arc<Mutex<Vec<String>>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut stream = BufReader::new(stream);
                let head = read_head(&mut stream).await;
                seen.lock().unwrap().extend(head.clone());
                // user:p@ss
                if !head.contains(&"Proxy-Authorization: Basic dXNlcjpwQHNz".to_owned()) {
                    let _ = stream
                        .get_mut()
                        .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                        .await;
                    return;
                }
                let target = head[0].split(' ').nth(1).unwrap().to_owned();
                if head[0].starts_with("CONNECT") {
                    let mut remote = TcpStream::connect(target).await.unwrap();
                    stream
                        .get_mut()
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(stream.get_mut(), &mut remote).await;
                } else {
                    stream
                        .get_mut()
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nproxy")
                        .await
                        .unwrap();
                }
            });
        }
    });
    address
}

/// 要用户名与密码（u / pw）的 SOCKS5 代理：只收 CONNECT，按请求里的域名或 IPv4 连过去。记下请求的主机。
async fn socks5_proxy(seen: Arc<Mutex<Vec<String>>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut greeting = [0; 2];
                stream.read_exact(&mut greeting).await.unwrap();
                let mut methods = vec![0; greeting[1] as usize];
                stream.read_exact(&mut methods).await.unwrap();
                assert!(methods.contains(&2), "要求用户名与密码认证");
                stream.write_all(&[5, 2]).await.unwrap();
                let version = stream.read_u8().await.unwrap();
                assert_eq!(version, 1);
                let mut user = vec![0; stream.read_u8().await.unwrap() as usize];
                stream.read_exact(&mut user).await.unwrap();
                let mut password = vec![0; stream.read_u8().await.unwrap() as usize];
                stream.read_exact(&mut password).await.unwrap();
                let ok = user == b"u" && password == b"pw";
                stream.write_all(&[1, u8::from(!ok)]).await.unwrap();
                if !ok {
                    return;
                }
                let mut request = [0; 4];
                stream.read_exact(&mut request).await.unwrap();
                let host = match request[3] {
                    1 => {
                        let mut ip = [0; 4];
                        stream.read_exact(&mut ip).await.unwrap();
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    3 => {
                        let mut name = vec![0; stream.read_u8().await.unwrap() as usize];
                        stream.read_exact(&mut name).await.unwrap();
                        String::from_utf8(name).unwrap()
                    }
                    other => panic!("不认得的地址类型 {other}"),
                };
                let port = stream.read_u16().await.unwrap();
                seen.lock().unwrap().push(host.clone());
                let mut remote = TcpStream::connect((host.as_str(), port)).await.unwrap();
                stream
                    .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut remote).await;
            });
        }
    });
    address
}

/// 照固定的路线开中转，连不过去的目标与原因记进 `failures`。
fn relay_reporting(
    upstream: Result<Upstream, String>,
    failures: Arc<Mutex<Vec<(String, String)>>>,
) -> Relay {
    let route: Route = Arc::new(move |_: &Target| upstream.clone());
    let report: Report = Arc::new(move |target: &Target, reason: &str| {
        failures
            .lock()
            .unwrap()
            .push((target.host.clone(), reason.to_owned()));
    });
    Relay::start(route, report).unwrap()
}

/// 照固定的路线开中转。
fn relay(upstream: Result<Upstream, String>) -> Relay {
    relay_reporting(upstream, Arc::new(Mutex::new(Vec::new())))
}

/// 经中转对 `target` 建 CONNECT 隧道，送一句话、读回同样的话。
async fn round_trip(relay: &Relay, target: &str) {
    let mut stream = BufReader::new(
        TcpStream::connect(("127.0.0.1", relay.port()))
            .await
            .unwrap(),
    );
    stream
        .get_mut()
        .write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let head = read_head(&mut stream).await;
    assert!(head[0].starts_with("HTTP/1.1 200"), "{head:?}");
    stream.get_mut().write_all(b"ping").await.unwrap();
    let mut answer = [0; 4];
    stream.read_exact(&mut answer).await.unwrap();
    assert_eq!(&answer, b"ping");
}

/// 经中转发一个写着完整地址的请求，返回响应全文。
async fn fetch(relay: &Relay, url: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", relay.port()))
        .await
        .unwrap();
    stream
        .write_all(
            format!("GET {url} HTTP/1.1\r\nHost: x\r\nProxy-Connection: keep-alive\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn direct_tunnels_and_forwards() {
    let echo = echo().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let origin = origin(seen.clone()).await;
    let relay = relay(Ok(Upstream::Direct));

    round_trip(&relay, &echo.to_string()).await;
    let response = fetch(&relay, &format!("http://{origin}/a/b?c=1")).await;
    assert!(response.ends_with("origin"), "{response}");
    // 发给目标的是路径，代理头去掉，要对方答完即关
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0], "GET /a/b?c=1 HTTP/1.1");
    assert!(!seen.iter().any(|line| line.starts_with("Proxy-Connection")));
    assert!(seen.contains(&"Connection: close".to_owned()));
}

#[tokio::test]
async fn an_upstream_http_proxy_gets_its_password() {
    let echo = echo().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let proxy = http_proxy(seen.clone()).await;
    let relay = relay(Ok(Upstream::Proxy(
        Url::parse(&format!("http://user:p%40ss@{proxy}")).unwrap(),
    )));

    round_trip(&relay, &echo.to_string()).await;
    let response = fetch(&relay, "http://video.example/a").await;
    assert!(response.ends_with("proxy"), "{response}");
    // 完整地址原样交给上游代理
    assert!(
        seen.lock()
            .unwrap()
            .contains(&"GET http://video.example/a HTTP/1.1".to_owned())
    );
}

#[tokio::test]
async fn an_upstream_socks5_proxy_gets_its_password() {
    let echo = echo().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let proxy = socks5_proxy(seen.clone()).await;
    let via =
        |scheme: &str| Upstream::Proxy(Url::parse(&format!("{scheme}://u:pw@{proxy}")).unwrap());

    // socks5h 把主机名交给代理；socks5 先在本机解析，交给代理的是地址
    round_trip(
        &relay(Ok(via("socks5h"))),
        &format!("localhost:{}", echo.port()),
    )
    .await;
    round_trip(&relay(Ok(via("socks5"))), &echo.to_string()).await;
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen, ["localhost", "127.0.0.1"]);
}

#[tokio::test]
async fn failures_are_answered_and_reported() {
    let failures = Arc::new(Mutex::new(Vec::new()));
    let relay = relay_reporting(Err("系统代理设置读不出".to_owned()), failures.clone());
    let response = fetch(&relay, "http://video.example/a").await;
    assert!(response.starts_with("HTTP/1.1 502"), "{response}");
    assert!(response.ends_with("系统代理设置读不出"), "{response}");
    assert_eq!(
        *failures.lock().unwrap(),
        [("video.example".to_owned(), "系统代理设置读不出".to_owned())]
    );

    // 连不上目标：同样回 502 并报出
    let failures = Arc::new(Mutex::new(Vec::new()));
    let relay = relay_reporting(Ok(Upstream::Direct), failures.clone());
    let response = fetch(&relay, "http://127.0.0.1:1/a").await;
    assert!(response.starts_with("HTTP/1.1 502"), "{response}");
    assert_eq!(failures.lock().unwrap()[0].0, "127.0.0.1");
}

#[tokio::test]
async fn a_dropped_relay_stops_listening() {
    let relay = relay(Ok(Upstream::Direct));
    let port = relay.port();
    drop(relay);
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
}
