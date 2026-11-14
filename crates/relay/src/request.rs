//! 读对端（网页引擎）发来的代理请求头，按去处改写。

use tokio::io::AsyncReadExt as _;
use tokio::net::TcpStream;
use url::{Host, Url};

/// 请求头最长多少字节：网页的请求头一般几 KB，超过的不是正常请求。
const MAX_HEAD: usize = 64 * 1024;

/// 一条连接要去的地方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// 与 [`Url::host`] 同样规范化（域名小写、IPv6 不带方括号）：可以直接与打开的地址的主机比较
    pub host: Host<String>,
    pub port: u16,
    /// 经 CONNECT 建隧道来的；否则是写着完整地址的 http 请求
    pub tunnel: bool,
}

impl Target {
    /// 「主机:端口」，IPv6 带方括号：CONNECT 请求的写法。
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// 连接用的主机名或地址（IPv6 不带方括号）。
    pub(crate) fn hostname(&self) -> String {
        match &self.host {
            Host::Domain(domain) => domain.clone(),
            Host::Ipv4(address) => address.to_string(),
            Host::Ipv6(address) => address.to_string(),
        }
    }
}

/// 读出的请求头。
pub(crate) struct Head {
    pub target: Target,
    method: String,
    /// http 请求的完整地址；CONNECT 时为 None
    url: Option<Url>,
    version: String,
    /// 请求行之后的各行原样（不含结尾的空行）
    lines: Vec<Vec<u8>>,
    /// 请求头之后已读到的字节
    pub rest: Vec<u8>,
}

pub(crate) enum HeadError {
    /// 对端没发完请求头就关了
    Closed,
    /// 不是认得的代理请求：原因
    Bad(String),
}

/// 读到请求头结束（空行）为止并解析；只收 CONNECT 与写着完整 `http://` 地址的请求。
pub(crate) async fn read_head(client: &mut TcpStream) -> Result<Head, HeadError> {
    let mut buffer = Vec::new();
    let end = loop {
        if let Some(at) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break at;
        }
        if buffer.len() > MAX_HEAD {
            return Err(HeadError::Bad(format!("请求头超过 {} KB", MAX_HEAD / 1024)));
        }
        let mut chunk = [0; 4096];
        match client.read(&mut chunk).await {
            Ok(0) | Err(_) => return Err(HeadError::Closed),
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    };
    let rest = buffer[end + 4..].to_vec();
    let mut lines = buffer[..end]
        .split(|&byte| byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line).to_vec());
    let request = lines.next().unwrap_or_default();
    let request =
        String::from_utf8(request).map_err(|_| HeadError::Bad("请求行不是 UTF-8".to_owned()))?;
    let mut parts = request.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(HeadError::Bad(format!("认不出的请求行：{request}")));
    };
    let tunnel = method.eq_ignore_ascii_case("CONNECT");
    let (host, port, url) = if tunnel {
        // CONNECT 的目标为「主机:端口」，端口必须写
        let (host, port) = target
            .rsplit_once(':')
            .ok_or_else(|| HeadError::Bad(format!("CONNECT 的目标要写端口：{target}")))?;
        let host =
            Host::parse(host).map_err(|_| HeadError::Bad(format!("认不出的主机：{target}")))?;
        let port = port
            .parse()
            .map_err(|_| HeadError::Bad(format!("认不出的端口：{target}")))?;
        (host, port, None)
    } else {
        let url =
            Url::parse(target).map_err(|_| HeadError::Bad(format!("认不出的目标：{target}")))?;
        if url.scheme() != "http" {
            return Err(HeadError::Bad(format!(
                "只转发 http 地址，{} 地址要用 CONNECT",
                url.scheme()
            )));
        }
        let host = url
            .host()
            .ok_or_else(|| HeadError::Bad(format!("目标没有主机：{target}")))?
            .to_owned();
        let port = url
            .port_or_known_default()
            .expect("http 地址总有端口（没写时为 80）");
        (host, port, Some(url))
    };
    Ok(Head {
        target: Target { host, port, tunnel },
        method: method.to_owned(),
        url,
        version: version.to_owned(),
        lines: lines.collect(),
        rest,
    })
}

impl Head {
    /// 发给目标本身（直连、经 SOCKS）的请求：请求行改写成路径，去掉只对代理的头与连接管理的头，要对方答完即关。
    pub fn for_origin(&self) -> Vec<u8> {
        let url = self.url.as_ref().expect("只对 http 请求改写");
        let mut path = url.path().to_owned();
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        self.rebuild(&path, None)
    }

    /// 发给上游 HTTP 代理的请求：保留完整地址，换成上游的口令 `auth`（`Basic …`，没有口令时不带）。
    pub fn for_proxy(&self, auth: Option<&str>) -> Vec<u8> {
        let url = self.url.as_ref().expect("只对 http 请求改写");
        self.rebuild(url.as_str(), auth)
    }

    fn rebuild(&self, target: &str, auth: Option<&str>) -> Vec<u8> {
        let mut out = format!("{} {target} {}\r\n", self.method, self.version).into_bytes();
        for line in &self.lines {
            if !hop_by_hop(line) {
                out.extend_from_slice(line);
                out.extend_from_slice(b"\r\n");
            }
        }
        if let Some(auth) = auth {
            out.extend_from_slice(format!("Proxy-Authorization: {auth}\r\n").as_bytes());
        }
        out.extend_from_slice(b"Connection: close\r\n\r\n");
        out.extend_from_slice(&self.rest);
        out
    }
}

/// 只对这一跳有效、转发时去掉的头：给本中转的代理头与连接管理的头（转发时统一为 Connection: close）。
fn hop_by_hop(line: &[u8]) -> bool {
    let name = line.split(|&byte| byte == b':').next().unwrap_or_default();
    [
        b"proxy-connection".as_slice(),
        b"proxy-authorization",
        b"connection",
        b"keep-alive",
    ]
    .iter()
    .any(|hop| name.trim_ascii().eq_ignore_ascii_case(hop))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_targets_keep_brackets_only_in_the_authority() {
        let target = Target {
            host: Host::parse("[::1]").unwrap(),
            port: 443,
            tunnel: true,
        };
        assert_eq!(target.authority(), "[::1]:443");
        assert_eq!(target.hostname(), "::1");
    }

    #[test]
    fn hop_by_hop_headers_are_not_forwarded() {
        assert!(hop_by_hop(b"Proxy-Connection: keep-alive"));
        assert!(hop_by_hop(b"connection : close"));
        assert!(!hop_by_hop(b"Cookie: a=b"));
    }
}
