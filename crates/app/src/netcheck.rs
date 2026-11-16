//! 检测 BT 用的网络：DHT（直连与经 BT 代理）、UDP 的 tracker、IPv6、端口映射与连入，各下结论，再给一句总的建议。只发
//! 握手类的包（DHT 的 ping 与 find_node、BEP 15 的 connect），不带任何种子的信息哈希。探测与下结论分开：各探测函数
//! 联网量出 [`Measured`]（由 [`crate::Service::check_network`] 汇总），[`report`] 只照它下结论。

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use hs_m3u8_core::Url;
use tokio::net::UdpSocket;

use crate::diagnosis::CheckLevel;
use crate::socks::{ProxyCheck, UdpRelay};

/// DHT 的引导节点（libtorrent 与常见客户端默认的几个）。
pub(crate) const DHT_BOOTSTRAP: [(&str, u16); 4] = [
    ("dht.libtorrent.org", 25401),
    ("router.bittorrent.com", 6881),
    ("router.utorrent.com", 6881),
    ("dht.transmissionbt.com", 6881),
];
/// 每一轮发出后等回应多久：DHT 节点与 tracker 正常都在一两秒内回。
const REPLY_WAIT: Duration = Duration::from_secs(4);
/// 从引导节点要来的普通节点最多 ping 多少个。
const NODES_PINGED: usize = 16;
/// 普通 DHT 节点里至少这么多比例回应才算 UDP 正常：节点表里常有已下线的，正常网络也回不满。
const HEALTHY_SHARE: f32 = 0.25;

/// 问了几个、回了几个。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ratio {
    pub asked: usize,
    pub replied: usize,
}

impl Ratio {
    fn healthy(self) -> bool {
        self.asked > 0 && self.replied as f32 >= self.asked as f32 * HEALTHY_SHARE
    }
}

/// 一次 DHT 探测量到的。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Dht {
    pub bootstrap: Ratio,
    /// 引导节点给的普通节点
    pub nodes: Ratio,
    /// 回应里写的「对方看到的本机地址」（BEP 42 的 ip）
    pub external: Option<IpAddr>,
}

/// 检测量到的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Measured {
    pub dht: Dht,
    /// UDP 的 tracker 握手；公共 tracker 列表还没取到时为 None
    pub trackers: Option<Ratio>,
    /// 本机的公网 IPv6 地址；没有时为 None
    pub ipv6: Option<Ipv6Addr>,
    /// IPv6 的 DHT 引导节点
    pub ipv6_dht: Ratio,
    /// BT 代理：去掉凭据的地址、探到的情况、经它的 DHT（用不了时为 None）；没设时为 None
    pub proxy: Option<(String, ProxyCheck, Option<Dht>)>,
    /// UPnP 或 NAT-PMP 映射到的外部端口；没映射成时为 None
    pub mapped: Option<u16>,
    /// 有过节点从外面连进来
    pub incoming: bool,
}

/// 检测的一项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetItem {
    Dht,
    UdpTrackers,
    Ipv6,
    Proxy,
    Incoming,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetCheck {
    pub item: NetItem,
    pub level: CheckLevel,
    pub text: String,
}

/// 检测的结果：各项与一句总的建议。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkReport {
    pub checks: Vec<NetCheck>,
    pub verdict: String,
}

/// 照量到的下结论。
pub(crate) fn report(measured: &Measured) -> NetworkReport {
    let check = |item, level, text: String| NetCheck { item, level, text };
    let dht = &measured.dht;
    let direct_ok = dht.nodes.healthy();
    let mut checks = vec![match () {
        () if dht.bootstrap.replied == 0 => check(
            NetItem::Dht,
            CheckLevel::Bad,
            format!(
                "DHT 的 {} 个引导节点一个都没回：UDP 多半被挡",
                dht.bootstrap.asked
            ),
        ),
        () if direct_ok => check(
            NetItem::Dht,
            CheckLevel::Ok,
            format!(
                "DHT 正常：{} 个节点里 {} 个回应",
                dht.nodes.asked, dht.nodes.replied
            ),
        ),
        () => check(
            NetItem::Dht,
            CheckLevel::Bad,
            format!(
                "DHT 大多不通：{} 个节点只回 {} 个（引导节点 {} 个回 {} 个），UDP 多半被运营商或路由器限制",
                dht.nodes.asked, dht.nodes.replied, dht.bootstrap.asked, dht.bootstrap.replied
            ),
        ),
    }];
    checks.push(match measured.trackers {
        None => check(
            NetItem::UdpTrackers,
            CheckLevel::Warn,
            "公共 tracker 列表没取到，没测 UDP 的 tracker".to_owned(),
        ),
        Some(ratio) if ratio.replied > 0 => check(
            NetItem::UdpTrackers,
            CheckLevel::Ok,
            format!(
                "UDP 的 tracker {} 个里 {} 个回应",
                ratio.asked, ratio.replied
            ),
        ),
        Some(ratio) => check(
            NetItem::UdpTrackers,
            CheckLevel::Bad,
            format!("UDP 的 tracker {} 个都没回", ratio.asked),
        ),
    });
    checks.push(match measured.ipv6 {
        None => check(
            NetItem::Ipv6,
            CheckLevel::Ok,
            "没有公网 IPv6 地址".to_owned(),
        ),
        Some(address) if measured.ipv6_dht.replied > 0 => check(
            NetItem::Ipv6,
            CheckLevel::Ok,
            format!("有公网 IPv6（{address}），IPv6 的 DHT 通"),
        ),
        Some(address) => check(
            NetItem::Ipv6,
            CheckLevel::Warn,
            format!(
                "有公网 IPv6（{address}），但 IPv6 的 UDP 不通：光猫或路由器的 IPv6 防火墙可能挡着，关掉它（多要光猫的超级管理员账号）后 IPv6 能直连"
            ),
        ),
    });
    let proxy_ok = matches!(&measured.proxy, Some((_, _, Some(dht))) if dht.nodes.healthy());
    checks.push(match &measured.proxy {
        None => check(
            NetItem::Proxy,
            CheckLevel::Ok,
            "没设 BT 代理".to_owned(),
        ),
        Some((_, ProxyCheck::Unusable(why), _)) => check(
            NetItem::Proxy,
            CheckLevel::Bad,
            format!("BT 代理用不了：{why}"),
        ),
        Some((address, _, Some(dht))) if dht.nodes.healthy() => check(
            NetItem::Proxy,
            CheckLevel::Ok,
            format!(
                "经 BT 代理 {address} 的 DHT 正常：{} 个节点里 {} 个回应",
                dht.nodes.asked, dht.nodes.replied
            ),
        ),
        Some((address, _, dht)) => check(
            NetItem::Proxy,
            CheckLevel::Warn,
            format!(
                "经 BT 代理 {address} 的 DHT 也大多不通（{} 个节点回 {} 个）：代理那边的网络可能也限制 UDP",
                dht.as_ref().map_or(0, |dht| dht.nodes.asked),
                dht.as_ref().map_or(0, |dht| dht.nodes.replied)
            ),
        ),
    });
    let seen = dht
        .external
        .map_or_else(String::new, |ip| format!("；DHT 节点看到的出口 IP 为 {ip}"));
    checks.push(match (measured.mapped, measured.incoming) {
        _ if measured.proxy.is_some() => check(
            NetItem::Incoming,
            CheckLevel::Ok,
            format!("经 BT 代理时不映射端口、不接受外部连入{seen}"),
        ),
        (_, true) => check(
            NetItem::Incoming,
            CheckLevel::Ok,
            format!("有节点从外面连进来过{seen}"),
        ),
        (Some(port), false) => check(
            NetItem::Incoming,
            CheckLevel::Ok,
            format!("路由器已映射端口（外部 {port}），还没有节点连进来{seen}"),
        ),
        (None, false) => check(
            NetItem::Incoming,
            CheckLevel::Warn,
            format!(
                "路由器没映射端口，外面连不进来{seen}。路由器管理页里的外网 IPv4 与这个出口 IP 不同时是运营商级 NAT，映射也没用"
            ),
        ),
    });
    let verdict = match (direct_ok, &measured.proxy, proxy_ok) {
        (true, _, _) => "网络正常：BT 能直接连".to_owned(),
        (false, Some(_), true) => "直连的 UDP 受限，经 BT 代理正常：保持开着 BT 代理".to_owned(),
        (false, Some(_), false) => {
            "直连与经 BT 代理的 UDP 都大多不通：DHT 用不了，公开网络里人少的磁力很难取到种子信息".to_owned()
        }
        (false, None, _) => {
            "直连的 UDP 大多不通：DHT 用不了，公开网络里人少的磁力很难取到种子信息。可在「BT 代理」设能转发 UDP 的 SOCKS5 代理"
                .to_owned()
        }
    };
    NetworkReport { checks, verdict }
}

/// 发 UDP 包的通道：直连，或经 SOCKS5 代理转发。
pub(crate) struct Udp {
    socket: UdpSocket,
    relay: Option<UdpRelay>,
}

impl Udp {
    /// 直连的 IPv4 或 IPv6 通道。
    pub async fn direct(v6: bool) -> std::io::Result<Udp> {
        let any: IpAddr = if v6 {
            Ipv6Addr::UNSPECIFIED.into()
        } else {
            Ipv4Addr::UNSPECIFIED.into()
        };
        Ok(Udp {
            socket: UdpSocket::bind((any, 0)).await?,
            relay: None,
        })
    }

    /// 经已谈好的 UDP 转发。
    pub async fn via(relay: UdpRelay) -> std::io::Result<Udp> {
        Ok(Udp {
            socket: UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?,
            relay: Some(relay),
        })
    }

    async fn send(&self, to: SocketAddr, data: &[u8]) {
        // 发不出（网络不可达等）即算那个节点没回，照常量下去
        let _unsent = match (&self.relay, to) {
            (None, _) => self.socket.send_to(data, to).await,
            (Some(relay), SocketAddr::V4(to)) => {
                let mut wrapped = vec![0, 0, 0, 1];
                wrapped.extend_from_slice(&to.ip().octets());
                wrapped.extend_from_slice(&to.port().to_be_bytes());
                wrapped.extend_from_slice(data);
                self.socket.send_to(&wrapped, relay.address).await
            }
            // 经代理只转发 IPv4（libtorrent 同样只用 IPv4）
            (Some(_), SocketAddr::V6(_)) => return,
        };
    }

    /// 收一个包，返回内容与真正的来源；经代理时去掉 SOCKS5 的头，认不出的包丢掉接着收。
    async fn recv(&self, buffer: &mut [u8]) -> std::io::Result<(Vec<u8>, SocketAddr)> {
        loop {
            let (len, from) = self.socket.recv_from(buffer).await?;
            let datagram = &buffer[..len];
            if self.relay.is_none() {
                return Ok((datagram.to_vec(), from));
            }
            if datagram.len() > 10 && datagram[3] == 1 {
                let ip = Ipv4Addr::new(datagram[4], datagram[5], datagram[6], datagram[7]);
                let port = u16::from_be_bytes([datagram[8], datagram[9]]);
                return Ok((datagram[10..].to_vec(), SocketAddr::new(ip.into(), port)));
            }
        }
    }

    /// 发出 `requests`（地址与内容），在 [`REPLY_WAIT`] 内收各地址的回应（每个地址算一次）。
    async fn exchange(&self, requests: &[(SocketAddr, Vec<u8>)]) -> Vec<(SocketAddr, Vec<u8>)> {
        for (to, data) in requests {
            self.send(*to, data).await;
        }
        let wanted: HashSet<SocketAddr> = requests.iter().map(|(to, _)| *to).collect();
        let mut replies = Vec::new();
        let mut seen = HashSet::new();
        let mut buffer = vec![0; 65536];
        let deadline = tokio::time::Instant::now() + REPLY_WAIT;
        while seen.len() < wanted.len() {
            let Ok(Ok((data, from))) =
                tokio::time::timeout_at(deadline, self.recv(&mut buffer)).await
            else {
                break;
            };
            if wanted.contains(&from) && seen.insert(from) {
                replies.push((from, data));
            }
        }
        replies
    }
}

/// 解析各引导节点的地址：`v6` 时取 IPv6 的（不含 IPv4 映射的），否则取 IPv4 的；解析不出的跳过。
pub(crate) async fn bootstrap(hosts: &[(&str, u16)], v6: bool) -> (usize, Vec<SocketAddr>) {
    let mut found = Vec::new();
    for (host, port) in hosts {
        if let Ok(addresses) = tokio::net::lookup_host((*host, *port)).await {
            let address = addresses.into_iter().find(|address| match address {
                SocketAddr::V4(_) => !v6,
                SocketAddr::V6(v6_address) => v6 && v6_address.ip().to_ipv4_mapped().is_none(),
            });
            found.extend(address);
        }
    }
    (hosts.len(), found)
}

/// DHT 的请求（KRPC）：`query` 为 `ping` 或 `find_node`（带随机的目标）。
fn krpc(query: &str) -> Vec<u8> {
    let mut message = b"d1:ad2:id20:".to_vec();
    message.extend(random20());
    if query == "find_node" {
        message.extend_from_slice(b"6:target20:");
        message.extend(random20());
    }
    message.extend_from_slice(format!("e1:q{}:{query}1:t2:hs1:y1:qe", query.len()).as_bytes());
    message
}

/// 20 个随机字节：节点 ID 与查找的目标，只用于这次探测，不需要密码学强度。
fn random20() -> [u8; 20] {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut state = seed as u64 ^ 0x9e37_79b9_7f4a_7c15;
    std::array::from_fn(|_| {
        // xorshift64
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state as u8
    })
}

/// 经 `udp` 探 DHT：向 `asked` 个引导节点里解析得出的 `bootstrap` 要节点，再 ping 它们给的（最多 [`NODES_PINGED`] 个）。
pub(crate) async fn dht(udp: &Udp, asked: usize, bootstrap: &[SocketAddr]) -> Dht {
    let find = krpc("find_node");
    let requests: Vec<_> = bootstrap.iter().map(|to| (*to, find.clone())).collect();
    let replies = udp.exchange(&requests).await;
    let mut external = None;
    let mut nodes = Vec::new();
    for (_, data) in &replies {
        let Some(message) = bencode::decode(data) else {
            continue;
        };
        external = external.or_else(|| message.get(b"ip").and_then(external_ip));
        if let Some(compact) = message
            .get(b"r")
            .and_then(|r| r.get(b"nodes"))
            .and_then(bencode::Value::bytes)
        {
            for node in compact.as_chunks::<26>().0 {
                let ip = Ipv4Addr::new(node[20], node[21], node[22], node[23]);
                let address = SocketAddr::new(ip.into(), u16::from_be_bytes([node[24], node[25]]));
                if !nodes.contains(&address) && !bootstrap.contains(&address) {
                    nodes.push(address);
                }
            }
        }
    }
    nodes.truncate(NODES_PINGED);
    let ping = krpc("ping");
    let requests: Vec<_> = nodes.iter().map(|to| (*to, ping.clone())).collect();
    let pinged = udp.exchange(&requests).await;
    for (_, data) in &pinged {
        if external.is_none() {
            external =
                bencode::decode(data).and_then(|message| message.get(b"ip").and_then(external_ip));
        }
    }
    Dht {
        bootstrap: Ratio {
            asked,
            replied: replies.len(),
        },
        nodes: Ratio {
            asked: nodes.len(),
            replied: pinged.len(),
        },
        external,
    }
}

/// BEP 42 的 ip：IPv4 或 IPv6 地址，后跟端口。
fn external_ip(value: &bencode::Value) -> Option<IpAddr> {
    match value.bytes()? {
        [a, b, c, d, _, _] => Some(Ipv4Addr::new(*a, *b, *c, *d).into()),
        compact if compact.len() == 18 => {
            let octets: [u8; 16] = compact[..16].try_into().ok()?;
            Some(Ipv6Addr::from(octets).into())
        }
        _ => None,
    }
}

/// 对 `trackers` 里 udp:// 开头、主机为 IP 的做 BEP 15 的 connect，看回不回。
pub(crate) async fn udp_trackers(udp: &Udp, trackers: &[String]) -> Ratio {
    let addresses: Vec<SocketAddr> = trackers
        .iter()
        .filter_map(|tracker| Url::parse(tracker).ok())
        .filter(|url| url.scheme() == "udp")
        .filter_map(|url| Some(SocketAddr::new(url.host_str()?.parse().ok()?, url.port()?)))
        .collect();
    let mut connect = 0x0417_2710_1980_u64.to_be_bytes().to_vec();
    connect.extend_from_slice(&0_u32.to_be_bytes());
    connect.extend_from_slice(&0x6873_646c_u32.to_be_bytes());
    let requests: Vec<_> = addresses.iter().map(|to| (*to, connect.clone())).collect();
    let replies = udp.exchange(&requests).await;
    Ratio {
        asked: addresses.len(),
        replied: replies
            .iter()
            .filter(|(_, data)| data.len() >= 16 && data[..4] == [0, 0, 0, 0])
            .count(),
    }
}

/// 本机的公网 IPv6 地址（2000::/3 里的）：向一个公网地址「连接」UDP 套接字（不发包）看系统选的源地址；没有时为 None。
pub(crate) async fn public_ipv6() -> Option<Ipv6Addr> {
    let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await.ok()?;
    socket
        .connect((
            Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888),
            53,
        ))
        .await
        .ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V6(address) if address.segments()[0] & 0xe000 == 0x2000 => Some(address),
        _ => None,
    }
}

/// 只认 DHT 回应要用到的那点 bencode：整数、字节串、列表、字典。
mod bencode {
    #[derive(Debug, PartialEq, Eq)]
    pub enum Value {
        Int(i64),
        Bytes(Vec<u8>),
        List(Vec<Value>),
        Dict(Vec<(Vec<u8>, Value)>),
    }

    impl Value {
        pub fn get(&self, key: &[u8]) -> Option<&Value> {
            match self {
                Value::Dict(entries) => entries
                    .iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value),
                _ => None,
            }
        }

        pub fn bytes(&self) -> Option<&[u8]> {
            match self {
                Value::Bytes(bytes) => Some(bytes),
                _ => None,
            }
        }
    }

    /// 解出整段；不是一个完整的 bencode 值（或后面还有多余的字节）时为 None。
    pub fn decode(data: &[u8]) -> Option<Value> {
        let (value, rest) = value(data, 0)?;
        rest.is_empty().then_some(value)
    }

    /// 嵌套最多这么深：DHT 的回应只有两三层，防恶意的包把栈撑爆。
    const DEPTH: usize = 16;

    fn value(data: &[u8], depth: usize) -> Option<(Value, &[u8])> {
        if depth > DEPTH {
            return None;
        }
        match data.first()? {
            b'i' => {
                let end = data.iter().position(|&b| b == b'e')?;
                let number = std::str::from_utf8(&data[1..end]).ok()?.parse().ok()?;
                Some((Value::Int(number), &data[end + 1..]))
            }
            b'l' => {
                let mut rest = &data[1..];
                let mut items = Vec::new();
                while *rest.first()? != b'e' {
                    let (item, next) = value(rest, depth + 1)?;
                    items.push(item);
                    rest = next;
                }
                Some((Value::List(items), &rest[1..]))
            }
            b'd' => {
                let mut rest = &data[1..];
                let mut entries = Vec::new();
                while *rest.first()? != b'e' {
                    let (Value::Bytes(key), next) = value(rest, depth + 1)? else {
                        return None;
                    };
                    let (item, next) = value(next, depth + 1)?;
                    entries.push((key, item));
                    rest = next;
                }
                Some((Value::Dict(entries), &rest[1..]))
            }
            b'0'..=b'9' => {
                let colon = data.iter().position(|&b| b == b':')?;
                let len: usize = std::str::from_utf8(&data[..colon]).ok()?.parse().ok()?;
                let start = colon + 1;
                let bytes = data.get(start..start.checked_add(len)?)?;
                Some((Value::Bytes(bytes.to_vec()), &data[start + len..]))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ratio(asked: usize, replied: usize) -> Ratio {
        Ratio { asked, replied }
    }

    fn measured() -> Measured {
        Measured {
            dht: Dht {
                bootstrap: ratio(4, 3),
                nodes: ratio(16, 9),
                external: Some(Ipv4Addr::new(203, 0, 113, 7).into()),
            },
            trackers: Some(ratio(17, 12)),
            ipv6: None,
            ipv6_dht: Ratio::default(),
            proxy: None,
            mapped: Some(51413),
            incoming: false,
        }
    }

    fn level(report: &NetworkReport, item: NetItem) -> CheckLevel {
        report.checks.iter().find(|c| c.item == item).unwrap().level
    }

    #[test]
    fn a_healthy_network_is_said_to_be_fine() {
        let report = report(&measured());
        assert!(report.checks.iter().all(|c| c.level == CheckLevel::Ok));
        assert!(report.verdict.contains("网络正常"));
    }

    #[test]
    fn restricted_udp_points_to_the_proxy() {
        // 这台机器上量到的：普通节点 16 个回 1 个，IPv6 有地址而 UDP 不通，路由器不映射
        let mut restricted = measured();
        restricted.dht = Dht {
            bootstrap: ratio(4, 1),
            nodes: ratio(16, 1),
            external: Some(Ipv4Addr::new(120, 229, 78, 85).into()),
        };
        restricted.trackers = Some(ratio(17, 4));
        restricted.ipv6 = Some("2409:8a55:2e48:2e0::1".parse().unwrap());
        restricted.mapped = None;
        let plain = report(&restricted);
        assert_eq!(level(&plain, NetItem::Dht), CheckLevel::Bad);
        assert_eq!(level(&plain, NetItem::Ipv6), CheckLevel::Warn);
        let incoming = &plain
            .checks
            .iter()
            .find(|c| c.item == NetItem::Incoming)
            .unwrap()
            .text;
        assert!(incoming.contains("120.229.78.85") && incoming.contains("运营商级 NAT"));
        assert!(plain.verdict.contains("BT 代理"));
        // 经代理的 DHT 正常
        let proxied_dht = Dht {
            bootstrap: ratio(4, 2),
            nodes: ratio(16, 8),
            external: None,
        };
        restricted.proxy = Some((
            "socks5://127.0.0.1:7897".into(),
            ProxyCheck::Usable,
            Some(proxied_dht),
        ));
        let proxied = report(&restricted);
        assert_eq!(level(&proxied, NetItem::Proxy), CheckLevel::Ok);
        assert!(proxied.verdict.contains("保持开着 BT 代理"));
        restricted.proxy = Some((
            "socks5://127.0.0.1:7897".into(),
            ProxyCheck::Unusable("代理不转发 UDP".into()),
            None,
        ));
        assert_eq!(level(&report(&restricted), NetItem::Proxy), CheckLevel::Bad);
    }

    #[test]
    fn bencode_reads_what_dht_replies_carry() {
        let reply = b"d2:ip6:\x78\xe5\x4e\x55\x1a\xe11:rd2:id20:aaaaaaaaaaaaaaaaaaaa5:nodes0:e1:t2:hs1:y1:re";
        let message = bencode::decode(reply).unwrap();
        assert_eq!(
            message.get(b"ip").and_then(external_ip),
            Some(Ipv4Addr::new(120, 229, 78, 85).into())
        );
        assert_eq!(bencode::decode(b"d1:a"), None);
        assert_eq!(bencode::decode(b"i1ee"), None);
        let deep = format!("{}{}", "l".repeat(64), "e".repeat(64));
        assert_eq!(bencode::decode(deep.as_bytes()), None);
    }

    /// 本机的假 DHT 节点：回 find_node 时给出 `nodes`，回 ping；`silent` 的不回。
    async fn fake_node(nodes: Vec<SocketAddr>, silent: bool) -> SocketAddr {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buffer = [0; 2048];
            while let Ok((len, from)) = socket.recv_from(&mut buffer).await {
                if silent {
                    continue;
                }
                let mut compact = Vec::new();
                for node in &nodes {
                    let SocketAddr::V4(node) = node else { continue };
                    compact.extend_from_slice(&[b'n'; 20]);
                    compact.extend_from_slice(&node.ip().octets());
                    compact.extend_from_slice(&node.port().to_be_bytes());
                }
                let find = buffer[..len].windows(9).any(|w| w == b"find_node");
                let mut reply =
                    b"d2:ip6:\x7f\x00\x00\x01\x00\x011:rd2:id20:bbbbbbbbbbbbbbbbbbbb".to_vec();
                if find {
                    reply.extend_from_slice(format!("5:nodes{}:", compact.len()).as_bytes());
                    reply.extend_from_slice(&compact);
                }
                reply.extend_from_slice(b"e1:t2:hs1:y1:re");
                let _sent = socket.send_to(&reply, from).await;
            }
        });
        address
    }

    #[tokio::test]
    async fn dht_probe_counts_who_answers() {
        let mut nodes = Vec::new();
        for silent in [false, false, true, true] {
            nodes.push(fake_node(Vec::new(), silent).await);
        }
        let router = fake_node(nodes.clone(), false).await;
        let udp = Udp::direct(false).await.unwrap();
        let measured = dht(&udp, 2, &[router]).await;
        assert_eq!(measured.bootstrap, ratio(2, 1));
        assert_eq!(measured.nodes, ratio(4, 2));
        assert_eq!(measured.external, Some(Ipv4Addr::LOCALHOST.into()));
    }
}
