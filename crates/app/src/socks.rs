//! BT 用的 SOCKS5 代理（RFC 1928、RFC 1929）：写入时的校验、交给 libtorrent 的写法，与开始用之前的探测。libtorrent
//! 经代理时只开一个 IPv4 的接口，代理拒绝转发 UDP、或回 IPv6 的转发地址时它不报错，DHT、uTP 与 UDP 的 tracker 就此
//! 不通；探测照它的步骤走一遍，把这类问题报出来。

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use hs_dlx_torrent::SocksProxy;
use hs_m3u8_core::Url;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// 探测最多等多久：解析、连接与三次往返都在本机或近处，过了即算代理没回应。
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// RFC 1929 的用户名与密码最长多少字节。
const CREDENTIAL_LIMIT: usize = 255;

/// BT 代理探到的情况。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProxyCheck {
    /// 直连，不经代理
    Direct,
    /// 设了代理，还没探（BT 会话建起时探）
    Unchecked,
    Checking,
    /// 连得上、认证过了、转发 UDP
    Usable,
    /// 用不了，为原因
    Unusable(String),
}

/// 地址里的用户名或密码解开百分号编码；不是 UTF-8 时为 None。
fn decoded(text: &str) -> Option<String> {
    percent_encoding::percent_decode_str(text)
        .decode_utf8()
        .ok()
        .map(|text| text.into_owned())
}

/// BT 代理的地址能不能用：`socks5://` 或 `socks5h://` 开头（两者一样，主机名交给代理解析），主机为 IPv4 地址或主机名
/// （不收 IPv6 地址：libtorrent 经代理时只开 IPv4 的接口），写明端口，只写主机与端口；用户名与密码可选，有密码就要有
/// 用户名，解开后为 UTF-8、各不超过 255 字节。不能用时为原因。
pub(crate) fn check(url: &Url) -> Result<(), String> {
    if !matches!(url.scheme(), "socks5" | "socks5h") {
        return Err(
            "BT 的代理要是 SOCKS5（socks5:// 开头）：HTTP 代理转发不了 DHT 与 UDP".to_owned(),
        );
    }
    match url.host_str() {
        // IPv6 地址写在方括号里
        Some(host) if host.starts_with('[') => {
            return Err("BT 的代理不能用 IPv6 地址，改用 IPv4 地址或主机名".to_owned());
        }
        Some(host) if !host.is_empty() && url.port().is_some() => {}
        _ => return Err("BT 的代理要写明主机与端口，如 socks5://127.0.0.1:1080".to_owned()),
    }
    if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        return Err("BT 的代理地址只写主机与端口（可带用户名与密码）".to_owned());
    }
    let username = decoded(url.username()).ok_or("BT 代理的用户名不是 UTF-8")?;
    let password = url
        .password()
        .map(decoded)
        .unwrap_or(Some(String::new()))
        .ok_or("BT 代理的密码不是 UTF-8")?;
    if username.is_empty() && !password.is_empty() {
        return Err("BT 代理有密码就要有用户名".to_owned());
    }
    if username.len() > CREDENTIAL_LIMIT || password.len() > CREDENTIAL_LIMIT {
        return Err(format!(
            "BT 代理的用户名与密码各不能超过 {CREDENTIAL_LIMIT} 字节"
        ));
    }
    Ok(())
}

/// 交给 libtorrent 的写法；None 为直连。`url` 已过 [`check`]；主机 `localhost` 换成 127.0.0.1（解析它常先得到
/// IPv6 的 ::1）。
pub(crate) fn libtorrent_proxy(url: Option<&Url>) -> SocksProxy {
    let Some(url) = url else {
        return SocksProxy::default();
    };
    let host = url.host_str().expect("存下的代理都过了 check，有主机");
    let host = if host.eq_ignore_ascii_case("localhost") {
        Ipv4Addr::LOCALHOST.to_string()
    } else {
        host.to_owned()
    };
    SocksProxy {
        host,
        port: url.port().expect("存下的代理都过了 check，有端口"),
        username: decoded(url.username()).expect("存下的代理都过了 check，用户名是 UTF-8"),
        password: url
            .password()
            .map(|password| decoded(password).expect("存下的代理都过了 check，密码是 UTF-8"))
            .unwrap_or_default(),
    }
}

/// 照 libtorrent 经代理时的步骤探 `url`（已过 [`check`]）：解析主机取 IPv4 地址、连上去、按地址里有没有用户名选认证
/// 方式、申请转发 UDP，看它回的转发地址是不是 IPv4。用不了时为原因；[`PROBE_TIMEOUT`] 内没做完也算用不了。
pub(crate) async fn probe(url: &Url) -> Result<(), String> {
    associate(url).await.map(drop)
}

/// 经代理转发 UDP 的通道：控制连接断开即失效。
pub(crate) struct UdpRelay {
    /// 控制连接：持有期间转发有效
    _control: TcpStream,
    /// 发往这里的包带上 SOCKS5 的 UDP 头（RFC 1928 第 7 节）由代理转发
    pub address: SocketAddr,
}

/// 照 [`probe`] 的步骤谈好 UDP 转发并留着控制连接；用不了时为原因。
pub(crate) async fn associate(url: &Url) -> Result<UdpRelay, String> {
    let proxy = libtorrent_proxy(Some(url));
    tokio::time::timeout(PROBE_TIMEOUT, handshake(&proxy))
        .await
        .map_err(|_| format!("代理 {} 10 秒内没回应", hs_m3u8_core::bare_url(url)))?
}

async fn handshake(proxy: &SocksProxy) -> Result<UdpRelay, String> {
    let at = format!("{}:{}", proxy.host, proxy.port);
    let address = tokio::net::lookup_host(&at)
        .await
        .map_err(|error| format!("解析不出代理 {at}：{error}"))?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| format!("代理 {at} 没有 IPv4 地址"))?;
    let mut stream = TcpStream::connect(address)
        .await
        .map_err(|error| format!("连不上代理 {at}：{error}"))?;
    let io = |error: std::io::Error| format!("代理 {at} 断开了：{error}");
    let login = !proxy.username.is_empty();
    let method = if login { 2 } else { 0 };
    stream.write_all(&[5, 1, method]).await.map_err(io)?;
    let mut reply = [0; 2];
    stream.read_exact(&mut reply).await.map_err(io)?;
    if reply[0] != 5 {
        return Err(format!("{at} 不是 SOCKS5 代理"));
    }
    if reply[1] != method {
        return Err(if login {
            format!("代理 {at} 不接受用户名与密码认证")
        } else {
            format!("代理 {at} 要认证：在地址里写上用户名与密码")
        });
    }
    if login {
        let mut request = vec![1, len(&proxy.username)];
        request.extend_from_slice(proxy.username.as_bytes());
        request.push(len(&proxy.password));
        request.extend_from_slice(proxy.password.as_bytes());
        stream.write_all(&request).await.map_err(io)?;
        stream.read_exact(&mut reply).await.map_err(io)?;
        if reply[1] != 0 {
            return Err(format!("代理 {at} 的用户名或密码不对"));
        }
    }
    // UDP ASSOCIATE，客户端地址不定（0.0.0.0:0），同 libtorrent
    stream
        .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
        .await
        .map_err(io)?;
    let mut head = [0; 4];
    stream.read_exact(&mut head).await.map_err(io)?;
    if head[1] != 0 {
        return Err(format!(
            "代理 {at} 不转发 UDP（{}）：DHT、uTP 与 UDP 的 tracker 经不了它",
            refusal(head[1])
        ));
    }
    match head[3] {
        1 => {
            let mut bound = [0; 6];
            stream.read_exact(&mut bound).await.map_err(io)?;
            let ip = Ipv4Addr::new(bound[0], bound[1], bound[2], bound[3]);
            let port = u16::from_be_bytes([bound[4], bound[5]]);
            // 回 0.0.0.0 即与代理同一个地址
            let ip = if ip.is_unspecified() {
                address.ip()
            } else {
                ip.into()
            };
            Ok(UdpRelay {
                _control: stream,
                address: SocketAddr::new(ip, port),
            })
        }
        4 => Err(format!(
            "代理 {at} 转发 UDP 用的是 IPv6 地址，经代理时只能用 IPv4：DHT、uTP 与 UDP 的 tracker 经不了它"
        )),
        other => Err(format!(
            "代理 {at} 回的 UDP 转发地址认不出（地址类型 {other}）"
        )),
    }
}

/// 用户名或密码的长度（已过 [`check`]，不超过 255）。
fn len(text: &str) -> u8 {
    u8::try_from(text.len()).expect("check 已限定不超过 255 字节")
}

/// SOCKS5 回的失败码的说法（RFC 1928 第 6 节）。
fn refusal(code: u8) -> String {
    let what = match code {
        1 => "代理出错",
        2 => "规则不允许",
        3 => "网络不可达",
        4 => "主机不可达",
        5 => "连接被拒",
        6 => "超时",
        7 => "不支持这个命令",
        8 => "不支持这种地址",
        _ => "未知的失败",
    };
    format!("回 {code}，{what}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn addresses_are_checked_where_they_are_written() {
        assert!(check(&url("socks5://127.0.0.1:7897")).is_ok());
        assert!(check(&url("socks5h://me:p%40ss@proxy.example:1080")).is_ok());
        let why = |text| check(&url(text)).unwrap_err();
        assert!(why("http://127.0.0.1:7897").contains("SOCKS5"));
        assert!(why("socks5://127.0.0.1").contains("端口"));
        assert!(why("socks5://[::1]:1080").contains("IPv6"));
        assert!(why("socks5://127.0.0.1:1080/x").contains("只写主机与端口"));
        assert!(why("socks5://:secret@127.0.0.1:1080").contains("用户名"));
        assert!(why("socks5://%FF@127.0.0.1:1080").contains("UTF-8"));
        let long = "a".repeat(256);
        assert!(why(&format!("socks5://{long}@127.0.0.1:1080")).contains("255"));
    }

    #[test]
    fn libtorrent_gets_decoded_credentials_and_ipv4_for_localhost() {
        let proxy = libtorrent_proxy(Some(&url("socks5h://me:p%40ss@LOCALHOST:1080")));
        assert_eq!(
            (
                proxy.host.as_str(),
                proxy.port,
                proxy.username.as_str(),
                proxy.password.as_str()
            ),
            ("127.0.0.1", 1080, "me", "p@ss")
        );
        assert_eq!(libtorrent_proxy(None), SocksProxy::default());
    }
}
