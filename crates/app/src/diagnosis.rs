//! BT 任务下得慢、下不动时的逐项诊断：监听、路由器的端口映射、有没有节点从外面连进来、DHT、tracker、公共 tracker、
//! 种子的人数与上传限速，每项一个结论与说明。这里只按查到的情况下结论，查由服务做（[`crate::Service::diagnose`]）。

use std::num::NonZeroU32;

/// 诊断的一项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckItem {
    /// 本机在不在监听、别人能不能连进来
    Listen,
    /// 路由器有没有把端口映射到这台电脑（UPnP、NAT-PMP）
    PortMapping,
    /// 有没有节点从外面连进来：没有多半在运营商内网（CGNAT）或被防火墙挡着
    Incoming,
    Dht,
    Trackers,
    /// 补没补上公共 tracker（ngosang/trackerslist 的列表，补给不是私有的种子）
    PublicTrackers,
    /// tracker 报的做种与在下的人数
    Swarm,
    Upload,
}

/// 一项的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckLevel {
    Ok,
    /// 可能拖慢，或还说不准
    Warn,
    /// 这一项不通
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub item: CheckItem,
    pub level: CheckLevel,
    /// 查到的情况与能做的事
    pub text: String,
}

/// 诊断时查到的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Facts {
    /// 在监听的端口；0 为没在监听
    pub listen_port: u16,
    pub listen_error: String,
    /// UPnP、NAT-PMP 映射到的外部端口（0 为没映射成）与最近一次失败的说明
    pub upnp_port: u16,
    pub upnp_error: String,
    pub natpmp_port: u16,
    pub natpmp_error: String,
    /// DHT 路由表里的节点数；None 为没统计到
    pub dht_nodes: Option<u64>,
    pub incoming: bool,
    pub private: bool,
    /// 在做种（下完了）：人数一项看有没有人要，不看凑不凑得齐
    pub seeding: bool,
    /// 这个种子连着的节点数
    pub peers: u32,
    pub trackers: Vec<Tracker>,
    /// 种子的做种与在下的人数（tracker 报的，取各处最多的）；None 为没报
    pub seeds: Option<u32>,
    pub leechers: Option<u32>,
    pub public: Public,
    /// 上传限速，字节每秒；None 为不限
    pub upload_limit: Option<NonZeroU32>,
}

/// 公共 tracker 的状况。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Public {
    /// 不补（没有列表的来源）
    Off,
    /// 列表还在取
    Fetching,
    /// 列表这次没取到，为原因
    Failed(String),
    /// 列表取到了，种子的 tracker 里有其中这么多个
    Added(usize),
}

/// 一个 tracker 的状况。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tracker {
    pub working: bool,
    /// 每个监听地址的通告都失败过，且没有成功的
    pub failed: bool,
    /// tracker 回的错误或连不上的原因；没有时为空
    pub message: String,
}

/// 按查到的下结论，顺序同 [`CheckItem`]。
pub(crate) fn checks(facts: &Facts) -> Vec<Check> {
    let check = |item, level, text: String| Check { item, level, text };
    let mut checks = Vec::new();
    checks.push(if facts.listen_port > 0 {
        check(
            CheckItem::Listen,
            CheckLevel::Ok,
            format!("在端口 {} 接受连入", facts.listen_port),
        )
    } else {
        let why = if facts.listen_error.is_empty() {
            String::new()
        } else {
            format!("（{}）", facts.listen_error)
        };
        check(
            CheckItem::Listen,
            CheckLevel::Bad,
            format!("没在监听{why}：别人连不进来，只能主动去连别人"),
        )
    });
    let listening = facts.listen_port > 0;
    let mapped = [("UPnP", facts.upnp_port), ("NAT-PMP", facts.natpmp_port)]
        .into_iter()
        .find(|(_, port)| *port > 0);
    let errors: Vec<String> = [
        ("UPnP", &facts.upnp_error),
        ("NAT-PMP", &facts.natpmp_error),
    ]
    .into_iter()
    .filter(|(_, error)| !error.is_empty())
    .map(|(via, error)| format!("{via}：{error}"))
    .collect();
    checks.push(match mapped {
        _ if !listening => check(
            CheckItem::PortMapping,
            CheckLevel::Warn,
            "没在监听，不请求路由器映射端口".to_owned(),
        ),
        Some((via, port)) => check(
            CheckItem::PortMapping,
            CheckLevel::Ok,
            format!("路由器已映射端口（{via}，外部 {port}）"),
        ),
        None if errors.is_empty() => check(
            CheckItem::PortMapping,
            CheckLevel::Warn,
            "路由器还没回应端口映射".to_owned(),
        ),
        None => check(
            CheckItem::PortMapping,
            CheckLevel::Warn,
            format!(
                "路由器没映射端口（{}）：在路由器上打开 UPnP，或把端口 {} 手动映射到这台电脑；不在路由器后面的不用管",
                errors.join("；"),
                facts.listen_port
            ),
        ),
    });
    checks.push(if facts.incoming {
        check(
            CheckItem::Incoming,
            CheckLevel::Ok,
            "有节点从外面连了进来".to_owned(),
        )
    } else if !listening {
        check(
            CheckItem::Incoming,
            CheckLevel::Warn,
            "没在监听，别人连不进来".to_owned(),
        )
    } else {
        check(
            CheckItem::Incoming,
            CheckLevel::Warn,
            "到现在还没有节点从外面连进来：可能在运营商内网（CGNAT）或被防火墙挡着，只能主动去连别人，能连上的会少很多"
                .to_owned(),
        )
    });
    checks.push(match (facts.private, facts.dht_nodes) {
        (true, _) => check(
            CheckItem::Dht,
            CheckLevel::Ok,
            "私有种子不用 DHT".to_owned(),
        ),
        (false, Some(0)) => check(
            CheckItem::Dht,
            CheckLevel::Warn,
            "DHT 里一个节点也没有：刚开始找要等一会儿；一直没有多半是网络挡了 UDP 或 DHT，只能靠 tracker 找节点"
                .to_owned(),
        ),
        (false, Some(nodes)) => check(
            CheckItem::Dht,
            CheckLevel::Ok,
            format!("DHT 里有 {nodes} 个节点"),
        ),
        (false, None) => check(
            CheckItem::Dht,
            CheckLevel::Warn,
            "没统计到 DHT 的节点数".to_owned(),
        ),
    });
    let total = facts.trackers.len();
    let working = facts.trackers.iter().filter(|t| t.working).count();
    let failed: Vec<&Tracker> = facts.trackers.iter().filter(|t| t.failed).collect();
    let why = failed
        .iter()
        .find(|t| !t.message.is_empty())
        .map_or_else(String::new, |t| format!("（{}）", t.message));
    checks.push(match () {
        () if total == 0 && facts.private => check(
            CheckItem::Trackers,
            CheckLevel::Bad,
            "私有种子里没有 tracker：找不到节点".to_owned(),
        ),
        () if total == 0 => check(
            CheckItem::Trackers,
            CheckLevel::Warn,
            "种子里没有 tracker，只靠 DHT 找节点".to_owned(),
        ),
        // libtorrent 找到一个能用的就不再挨个试别的，回应了的往往只有一个
        () if working > 0 => check(
            CheckItem::Trackers,
            CheckLevel::Ok,
            format!("tracker 正常（{working} 个回应了）"),
        ),
        () if failed.len() == total => check(
            CheckItem::Trackers,
            CheckLevel::Bad,
            format!("{total} 个 tracker 都连不上{why}"),
        ),
        () if !failed.is_empty() => check(
            CheckItem::Trackers,
            CheckLevel::Warn,
            format!("{} 个 tracker 连不上{why}，其余的还没回应", failed.len()),
        ),
        () => check(
            CheckItem::Trackers,
            CheckLevel::Warn,
            format!("{total} 个 tracker 还没回应"),
        ),
    });
    checks.push(match &facts.public {
        _ if facts.private => check(
            CheckItem::PublicTrackers,
            CheckLevel::Ok,
            "私有种子不补公共 tracker".to_owned(),
        ),
        Public::Off => check(
            CheckItem::PublicTrackers,
            CheckLevel::Ok,
            "不补公共 tracker".to_owned(),
        ),
        Public::Fetching => check(
            CheckItem::PublicTrackers,
            CheckLevel::Warn,
            "公共 tracker 列表还在取".to_owned(),
        ),
        Public::Failed(why) => check(
            CheckItem::PublicTrackers,
            CheckLevel::Warn,
            format!(
                "公共 tracker 列表没取到（{why}）：隔一阵再取；取到之前只靠种子里的 tracker 与 DHT 找节点，可检查代理设置"
            ),
        ),
        Public::Added(0) => check(
            CheckItem::PublicTrackers,
            CheckLevel::Warn,
            "公共 tracker 列表取到了，还没补上".to_owned(),
        ),
        Public::Added(added) => check(
            CheckItem::PublicTrackers,
            CheckLevel::Ok,
            format!("补了 {added} 个公共 tracker"),
        ),
    });
    let counted = |seeds: u32, leechers: Option<u32>| {
        format!(
            "tracker 报做种 {seeds} 人{}",
            leechers.map_or_else(String::new, |n| format!("、在下 {n} 人"))
        )
    };
    checks.push(match (facts.seeding, facts.seeds, facts.leechers) {
        (true, _, Some(0)) => check(
            CheckItem::Swarm,
            CheckLevel::Ok,
            "tracker 报现在没人在下：暂时没人要你传".to_owned(),
        ),
        (true, Some(seeds), leechers) => {
            check(CheckItem::Swarm, CheckLevel::Ok, counted(seeds, leechers))
        }
        (true, None, _) => check(
            CheckItem::Swarm,
            CheckLevel::Ok,
            "tracker 没报人数".to_owned(),
        ),
        (false, Some(0), Some(0)) if facts.peers > 0 => check(
            CheckItem::Swarm,
            CheckLevel::Warn,
            format!(
                "tracker 报没人，可连着 {} 个节点：多半是 tracker 不统计这个种子",
                facts.peers
            ),
        ),
        (false, Some(0), Some(0)) => check(
            CheckItem::Swarm,
            CheckLevel::Bad,
            "tracker 报现在没人有这个种子".to_owned(),
        ),
        (false, Some(0), leechers) => check(
            CheckItem::Swarm,
            CheckLevel::Warn,
            format!(
                "tracker 报现在没人做种{}：能不能下完要看在下的人凑不凑得齐",
                leechers.map_or_else(String::new, |n| format!("（在下的 {n} 人）"))
            ),
        ),
        (false, Some(seeds), leechers) => {
            check(CheckItem::Swarm, CheckLevel::Ok, counted(seeds, leechers))
        }
        (false, None, _) => check(
            CheckItem::Swarm,
            if facts.peers > 0 {
                CheckLevel::Ok
            } else {
                CheckLevel::Warn
            },
            format!("tracker 没报人数，连着 {} 个节点", facts.peers),
        ),
    });
    checks.push(match facts.upload_limit {
        Some(limit) => check(
            CheckItem::Upload,
            CheckLevel::Ok,
            format!("上传限速 {} KB/s", limit.get() >> 10),
        ),
        None => check(CheckItem::Upload, CheckLevel::Ok, "上传不限速".to_owned()),
    });
    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Facts {
        Facts {
            listen_port: 51413,
            listen_error: String::new(),
            upnp_port: 51413,
            upnp_error: String::new(),
            natpmp_port: 0,
            natpmp_error: String::new(),
            dht_nodes: Some(320),
            incoming: true,
            private: false,
            seeding: false,
            peers: 5,
            trackers: vec![Tracker {
                working: true,
                failed: false,
                message: String::new(),
            }],
            seeds: Some(12),
            leechers: Some(3),
            public: Public::Added(20),
            upload_limit: None,
        }
    }

    fn level(facts: &Facts, item: CheckItem) -> CheckLevel {
        checks(facts)
            .into_iter()
            .find(|check| check.item == item)
            .unwrap()
            .level
    }

    #[test]
    fn a_healthy_session_passes_every_check() {
        assert!(
            checks(&healthy())
                .iter()
                .all(|check| check.level == CheckLevel::Ok)
        );
    }

    #[test]
    fn problems_are_told_apart() {
        let mut facts = healthy();
        facts.listen_port = 0;
        facts.listen_error = "address in use".into();
        assert_eq!(level(&facts, CheckItem::Listen), CheckLevel::Bad);
        // 没在监听：映射与连入无从判断
        facts.incoming = false;
        let text = |facts: &Facts, item| {
            checks(facts)
                .into_iter()
                .find(|check| check.item == item)
                .unwrap()
                .text
        };
        assert!(text(&facts, CheckItem::PortMapping).contains("没在监听"));
        assert!(text(&facts, CheckItem::Incoming).contains("没在监听"));
        let mut facts = healthy();
        facts.upnp_port = 0;
        facts.upnp_error = "no router".into();
        assert_eq!(level(&facts, CheckItem::PortMapping), CheckLevel::Warn);
        let mapping = text(&facts, CheckItem::PortMapping);
        assert!(mapping.contains("no router") && mapping.contains("51413"));
        let mut facts = healthy();
        facts.dht_nodes = Some(0);
        assert_eq!(level(&facts, CheckItem::Dht), CheckLevel::Warn);
        facts.private = true;
        assert_eq!(level(&facts, CheckItem::Dht), CheckLevel::Ok);
        // tracker：都连不上、一部分连不上、还没回应
        let mut facts = healthy();
        let tracker = |failed| Tracker {
            working: false,
            failed,
            message: if failed {
                "timed out".into()
            } else {
                String::new()
            },
        };
        facts.trackers = vec![tracker(true), tracker(true)];
        assert_eq!(level(&facts, CheckItem::Trackers), CheckLevel::Bad);
        assert!(text(&facts, CheckItem::Trackers).contains("timed out"));
        facts.trackers = vec![tracker(true), tracker(false)];
        assert_eq!(level(&facts, CheckItem::Trackers), CheckLevel::Warn);
        assert!(text(&facts, CheckItem::Trackers).contains("其余的还没回应"));
        facts.trackers = vec![tracker(false)];
        assert!(text(&facts, CheckItem::Trackers).contains("还没回应"));
        // 公共 tracker：没取到时带上原因；私有种子不补
        let mut facts = healthy();
        facts.public = Public::Failed("connection refused".into());
        assert_eq!(level(&facts, CheckItem::PublicTrackers), CheckLevel::Warn);
        assert!(text(&facts, CheckItem::PublicTrackers).contains("connection refused"));
        facts.private = true;
        assert_eq!(level(&facts, CheckItem::PublicTrackers), CheckLevel::Ok);
        // 人数：没人且没连着节点为不通；tracker 报没人而连着节点的不算
        let mut facts = healthy();
        facts.seeds = Some(0);
        facts.leechers = Some(0);
        assert_eq!(level(&facts, CheckItem::Swarm), CheckLevel::Warn);
        facts.peers = 0;
        assert_eq!(level(&facts, CheckItem::Swarm), CheckLevel::Bad);
        facts.leechers = Some(4);
        assert_eq!(level(&facts, CheckItem::Swarm), CheckLevel::Warn);
        // 做种时没人在下不算问题
        facts.seeding = true;
        facts.leechers = Some(0);
        assert_eq!(level(&facts, CheckItem::Swarm), CheckLevel::Ok);
        let mut facts = healthy();
        facts.incoming = false;
        assert_eq!(level(&facts, CheckItem::Incoming), CheckLevel::Warn);
    }
}
