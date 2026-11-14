//! 本机的 HTTP 代理中转：给网页引擎一个不带口令、只在本机回环地址上的 HTTP 代理，每条连接按调用方给的路线
//! （[`Route`]）直连目标，或经上游的 HTTP、HTTPS、SOCKS4(a)、SOCKS5(h) 代理（可带用户名与密码）连过去。网页引擎
//! 只能设不带口令的 HTTP 或 SOCKS5 代理、有的平台不能单独设直连，经它就都能照应用的代理设置走。
//!
//! 收两种请求：`CONNECT 主机:端口`（建隧道，HTTPS 与 macOS 上全部请求都这样来），与写着完整地址的 `http://` 请求
//! （改写成路径后发给目标，或原样发给上游 HTTP 代理；每条连接只转一个请求，转完即关）。连上之前失败时回 502，正文
//! 为原因；隧道建好之后出错只断开连接（网页引擎报它自己的网络错误）。
//!
//! 简化：只听本机回环地址、不核对来者，同一台机器上的别的进程（含别的登录用户）也能经它连出去，用上设置里带口令的
//! 上游代理。同一用户的进程本就读得到存储里的口令；多用户的机器上要防时，按对端端口查出连接所属的用户（各平台的
//! TCP 连接表带进程或用户），不是本用户的直接关掉。

mod request;
mod upstream;

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use url::Url;

pub use self::request::Target;

/// 读请求头最多多久：对端连上了却不说话时不让连接一直占着。
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

/// 一条连接经哪里走。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upstream {
    Direct,
    /// 经这个代理：协议为 http、https、socks4、socks4a、socks5、socks5h，可带用户名与密码
    Proxy(Url),
}

/// 每条连接经哪里走：按目标决定；决定不了时为原因，回给对端 502。在中转的线程上同步调用，要尽快返回、不能阻塞。
pub type Route = Arc<dyn Fn(&Target) -> Result<Upstream, String> + Send + Sync>;

/// 连不过去（路线决定不了、连不上目标或上游代理）时调，带上目标与原因：网页引擎拿到 502 时多半不报原因，调用方据此
/// 告诉用户。在中转的线程上调，要尽快返回。
pub type Report = Arc<dyn Fn(&Target, &str) + Send + Sync>;

/// 在后台线程上跑的中转；释放即停下，在途的连接随之断开（释放等线程退出，不等在途的域名解析）。
pub struct Relay {
    port: u16,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Relay {
    /// 在 127.0.0.1 的一个空闲端口上开始中转，每条连接照 `route` 走，连不过去时调 `report`；在名为 hs-m3u8-relay 的
    /// 后台线程上跑。建不起监听时为原因。
    pub fn start(route: Route, report: Report) -> Result<Relay, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("起不了代理中转的运行时：{error}"))?;
        // 在调用方的线程上绑定（调用方可能已在别的运行时里，不能在这里等待），交给中转自己的运行时
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .and_then(|listener| {
                listener.set_nonblocking(true)?;
                let _entered = runtime.enter();
                TcpListener::from_std(listener)
            })
            .map_err(|error| format!("代理中转建不起监听：{error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("读不出代理中转的端口：{error}"))?
            .port();
        let (stop, stopped) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("hs-m3u8-relay".to_owned())
            .spawn(move || {
                runtime.block_on(async move {
                    tokio::select! {
                        () = accept_all(&listener, &route, &report) => {}
                        // 释放 Relay 时发，或它没发就被释放：都停
                        _ = stopped => {}
                    }
                });
                // 在途连接的任务一并取消；阻塞池里还在做的域名解析不等它做完（解析不能取消，慢时可达几十秒）
                runtime.shutdown_background();
            })
            .map_err(|error| format!("起不了代理中转的线程：{error}"))?;
        Ok(Relay {
            port,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// 监听的端口（在 127.0.0.1 上）。
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            // 线程已自己停下（收连接一直出错不会停，只有它 panic）时没有人收
            let _stopped = stop.send(());
        }
        if let Some(thread) = self.thread.take()
            && let Err(panic) = thread.join()
        {
            std::panic::resume_unwind(panic);
        }
    }
}

/// 一个接一个地收连接，各在自己的任务里处理。收连接出错（多为文件描述符用尽）时隔一秒再收。
async fn accept_all(listener: &TcpListener, route: &Route, report: &Report) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let (route, report) = (route.clone(), report.clone());
                // 任务归运行时所有，中转停下时随运行时取消
                tokio::spawn(async move { serve(stream, &route, &report).await });
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
}

/// 处理一条连接：读请求头、按路线连上目标、转发。出错时能回就回 502 与原因，回不了（对端已断）就关掉。
async fn serve(mut client: TcpStream, route: &Route, report: &Report) {
    let head = match tokio::time::timeout(HEAD_TIMEOUT, request::read_head(&mut client)).await {
        Ok(Ok(head)) => head,
        Ok(Err(request::HeadError::Closed)) | Err(_) => return,
        Ok(Err(request::HeadError::Bad(reason))) => {
            reply(&mut client, "400 Bad Request", &reason).await;
            return;
        }
    };
    let target = head.target.clone();
    let connected = match route(&target) {
        Ok(upstream) => open(&head, &upstream).await,
        Err(reason) => Err(reason),
    };
    let (mut remote, outgoing) = match connected {
        Ok(connected) => connected,
        Err(reason) => {
            report(&target, &reason);
            reply(&mut client, "502 Bad Gateway", &reason).await;
            return;
        }
    };
    // 连上了：之后出错即连接断了，两边都关掉，网页引擎报它自己的网络错误
    if target.tunnel
        && client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_err()
    {
        return;
    }
    if remote.write_all(&outgoing).await.is_err() {
        return;
    }
    let _ended = tokio::io::copy_bidirectional(&mut client, &mut remote).await;
}

/// 照 `upstream` 连上去，并给出连上后先发过去的字节。CONNECT：连上目标（经 HTTP 代理时建好隧道），先发请求头之后
/// 已读到的、隧道里的字节。写着完整地址的 http 请求：直连或经 SOCKS 时连上目标、发改写成路径的请求；经 HTTP(S)
/// 代理时连上代理、原样（换上口令）发给它；都要对方答完即关（Connection: close）。
async fn open(
    head: &request::Head,
    upstream: &Upstream,
) -> Result<(upstream::Stream, Vec<u8>), String> {
    match upstream {
        Upstream::Proxy(proxy) if !head.target.tunnel && upstream::is_http(proxy) => Ok((
            upstream::open_proxy(proxy).await?,
            head.for_proxy(upstream::basic_auth(proxy).as_deref()),
        )),
        Upstream::Direct | Upstream::Proxy(_) => {
            let remote = upstream::connect(upstream, &head.target).await?;
            let outgoing = if head.target.tunnel {
                head.rest.clone()
            } else {
                head.for_origin()
            };
            Ok((remote, outgoing))
        }
    }
}

/// 回一个只有状态行与原因的响应。对端已断时什么也不做。
async fn reply(client: &mut TcpStream, status: &str, reason: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reason}",
        reason.len()
    );
    // 对端已断时没有人收
    let _gone = client.write_all(response.as_bytes()).await;
}
