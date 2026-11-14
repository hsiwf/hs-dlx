//! 连上目标：直连，或经上游的 HTTP、HTTPS（到代理这一段走 TLS）、SOCKS4(a)、SOCKS5(h) 代理。

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use percent_encoding::percent_decode_str;
use rustls_platform_verifier::ConfigVerifierExt as _;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_socks::tcp::{Socks4Stream, Socks5Stream};
use url::Url;

use crate::{Target, Upstream};

/// 连上目标（含与上游代理握手）最多多久：与 hs-m3u8-core 的默认连接超时相同。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 上游 HTTP 代理回 CONNECT 的响应头最长多少字节。
const MAX_REPLY: usize = 16 * 1024;
/// 没写端口的 SOCKS 代理用的端口（约定俗成的默认端口）。
const SOCKS_PORT: u16 = 1080;

/// 连到目标的字节流。
pub(crate) type Stream = Box<dyn Io>;

pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// 照 `upstream` 连上 `target`：经 HTTP(S) 代理时用 CONNECT 建隧道。连不上、上游拒绝、超时时为原因。
pub(crate) async fn connect(upstream: &Upstream, target: &Target) -> Result<Stream, String> {
    tokio::time::timeout(CONNECT_TIMEOUT, connect_now(upstream, target))
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "{} 秒没能连上 {}:{}",
                CONNECT_TIMEOUT.as_secs(),
                target.host,
                target.port
            ))
        })
}

async fn connect_now(upstream: &Upstream, target: &Target) -> Result<Stream, String> {
    let host = target.host.as_str();
    let port = target.port;
    let proxy = match upstream {
        Upstream::Direct => {
            let stream = TcpStream::connect((host, port))
                .await
                .map_err(|error| format!("连不上 {host}:{port}：{error}"))?;
            return Ok(Box::new(stream));
        }
        Upstream::Proxy(proxy) => proxy,
    };
    let proxy_host = proxy
        .host_str()
        .ok_or_else(|| "代理地址没有主机".to_owned())?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let socks = (proxy_host, proxy.port().unwrap_or(SOCKS_PORT));
    let (user, password) = credentials(proxy)?;
    let failed = |error: tokio_socks::Error| format!("经 SOCKS 代理连不上 {host}:{port}：{error}");
    let stream: Stream = match proxy.scheme() {
        "http" | "https" => {
            let mut stream = open_proxy(proxy).await?;
            handshake(&mut stream, target, basic_auth(proxy).as_deref()).await?;
            stream
        }
        // socks5h 与 socks4a 把主机名交给代理解析；socks5 与 socks4 先在本机解析
        "socks5h" => Box::new(
            socks5(socks, (host, port), &user, &password)
                .await
                .map_err(failed)?,
        ),
        "socks5" => {
            let address = resolve(host, port, false).await?;
            Box::new(
                socks5(socks, address, &user, &password)
                    .await
                    .map_err(failed)?,
            )
        }
        "socks4a" => Box::new(
            Socks4Stream::connect_with_userid(socks, (host, port), &user)
                .await
                .map_err(failed)?,
        ),
        "socks4" => {
            let address = resolve(host, port, true).await?;
            Box::new(
                Socks4Stream::connect_with_userid(socks, address, &user)
                    .await
                    .map_err(failed)?,
            )
        }
        other => return Err(format!("不认得的代理协议 {other}")),
    };
    Ok(stream)
}

async fn socks5(
    proxy: (&str, u16),
    target: impl tokio_socks::IntoTargetAddr<'_>,
    user: &str,
    password: &str,
) -> Result<Socks5Stream<TcpStream>, tokio_socks::Error> {
    if user.is_empty() && password.is_empty() {
        Socks5Stream::connect(proxy, target).await
    } else {
        Socks5Stream::connect_with_password(proxy, target, user, password).await
    }
}

/// 在本机把 `host` 解析成地址；`ipv4` 时只要 IPv4（SOCKS4 只能带 IPv4）。
async fn resolve(host: &str, port: u16, ipv4: bool) -> Result<std::net::SocketAddr, String> {
    tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| format!("解析不了 {host}：{error}"))?
        .find(|address| !ipv4 || address.is_ipv4())
        .ok_or_else(|| format!("{host} 没有{}地址", if ipv4 { " IPv4 " } else { "" }))
}

/// 上游是 HTTP 或 HTTPS 代理。
pub(crate) fn is_http(proxy: &Url) -> bool {
    matches!(proxy.scheme(), "http" | "https")
}

/// 连上上游 HTTP(S) 代理本身：https 的这一段走 TLS，按系统的证书校验代理的证书。
pub(crate) async fn open_proxy(proxy: &Url) -> Result<Stream, String> {
    let host = proxy
        .host_str()
        .ok_or_else(|| "代理地址没有主机".to_owned())?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = proxy
        .port_or_known_default()
        .ok_or_else(|| "代理地址没有端口".to_owned())?;
    let stream = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|error| format!("连不上代理 {host}:{port}：{error}"))?;
    if proxy.scheme() != "https" {
        return Ok(Box::new(stream));
    }
    let config = tls_config()?;
    let name = rustls::pki_types::ServerName::try_from(host.clone())
        .map_err(|_| format!("代理的主机名 {host} 不能用于 TLS"))?;
    let stream = tokio_rustls::TlsConnector::from(config)
        .connect(name, stream)
        .await
        .map_err(|error| format!("与代理 {host} 建不起 TLS：{error}"))?;
    Ok(Box::new(stream))
}

/// 连 HTTPS 代理用的 TLS 配置，进程里建一次。
fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            rustls::ClientConfig::with_platform_verifier()
                .map(Arc::new)
                .map_err(|error| format!("建不起 TLS 配置：{error}"))
        })
        .clone()
}

/// 经 HTTP 代理的 CONNECT 握手：要到 2xx 才算通。
async fn handshake(stream: &mut Stream, target: &Target, auth: Option<&str>) -> Result<(), String> {
    let authority = format!(
        "{}:{}",
        target.url().host_str().unwrap_or_default(),
        target.port
    );
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = auth {
        request.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("向代理发 CONNECT 时出错：{error}"))?;
    // 一个字节一个字节地读到空行：不能多读，之后的字节属于隧道
    let mut reply = Vec::new();
    while !reply.ends_with(b"\r\n\r\n") {
        if reply.len() > MAX_REPLY {
            return Err("代理回的响应头太长".to_owned());
        }
        let byte = stream
            .read_u8()
            .await
            .map_err(|error| format!("读代理的回应时出错：{error}"))?;
        reply.push(byte);
    }
    let status = reply
        .split(|&byte| byte == b'\r')
        .next()
        .map(String::from_utf8_lossy)
        .unwrap_or_default()
        .into_owned();
    let code = status.split(' ').nth(1).unwrap_or_default();
    if code.len() == 3 && code.starts_with('2') {
        Ok(())
    } else {
        Err(format!("代理拒绝建隧道：{status}"))
    }
}

/// 上游代理地址里的用户名与密码（解掉百分号编码）；没写时为空。不是 UTF-8 时为原因（SOCKS 认证要文本）。
fn credentials(proxy: &Url) -> Result<(String, String), String> {
    let decode = |text: &str| {
        percent_decode_str(text)
            .decode_utf8()
            .map(|text| text.into_owned())
            .map_err(|_| "代理的用户名或密码不是 UTF-8".to_owned())
    };
    Ok((
        decode(proxy.username())?,
        decode(proxy.password().unwrap_or_default())?,
    ))
}

/// 上游 HTTP(S) 代理的 Basic 认证头的值；地址里没写用户名与密码时为 None。
pub(crate) fn basic_auth(proxy: &Url) -> Option<String> {
    if proxy.username().is_empty() && proxy.password().is_none() {
        return None;
    }
    let mut pair: Vec<u8> = percent_decode_str(proxy.username()).collect();
    pair.push(b':');
    pair.extend(percent_decode_str(proxy.password().unwrap_or_default()));
    Some(format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(pair)
    ))
}
