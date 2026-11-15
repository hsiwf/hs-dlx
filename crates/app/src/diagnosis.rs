//! BT 任务下得慢、下不动时的逐项诊断：监听、路由器的端口映射、有没有节点从外面连进来、DHT、tracker、种子的人数与上传
//! 限速，每项一个结论与说明。这里只按查到的情况下结论，查由服务做（[`crate::Service::diagnose`]）。

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
    pub trackers: Vec<Tracker>,
    /// 种子的做种与在下的人数（tracker 报的，取各处最多的）；None 为没报
    pub seeds: Option<u32>,
    pub leechers: Option<u32>,
    /// 上传限速，字节每秒；None 为不限
    pub upload_limit: Option<NonZeroU32>,
}

/// 一个 tracker 的状况。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tracker {
    pub working: bool,
    pub fails: u32,
    /// tracker 回的错误或连不上的原因；没有时为空
    pub message: String,
}

/// 上传限速低于这个（字节每秒）时提醒：BT 的节点多按你传给它的多少回传。
const LOW_UPLOAD: u32 = 100 << 10;

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
            CheckLevel::Bad,
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
    let failing: Vec<&Tracker> = facts.trackers.iter().filter(|t| t.fails > 0).collect();
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
        () if working > 0 => check(
            CheckItem::Trackers,
            CheckLevel::Ok,
            format!("{working} / {total} 个 tracker 正常"),
        ),
        () if failing.len() == total => {
            let why = failing
                .iter()
                .find(|t| !t.message.is_empty())
                .map_or_else(String::new, |t| format!("：{}", t.message));
            check(
                CheckItem::Trackers,
                CheckLevel::Bad,
                format!("{total} 个 tracker 都连不上{why}"),
            )
        }
        () => check(
            CheckItem::Trackers,
            CheckLevel::Warn,
            format!("{total} 个 tracker 还没回应"),
        ),
    });
    checks.push(match (facts.seeds, facts.leechers) {
        (Some(0), Some(0)) => check(
            CheckItem::Swarm,
            CheckLevel::Bad,
            "tracker 报现在没人有这个种子".to_owned(),
        ),
        (Some(0), leechers) => check(
            CheckItem::Swarm,
            CheckLevel::Warn,
            format!(
                "tracker 报现在没人做种{}：能不能下完要看在下的人凑不凑得齐",
                leechers.map_or_else(String::new, |n| format!("（在下的 {n} 人）"))
            ),
        ),
        (Some(seeds), leechers) => check(
            CheckItem::Swarm,
            CheckLevel::Ok,
            format!(
                "tracker 报做种 {seeds} 人{}",
                leechers.map_or_else(String::new, |n| format!("、在下 {n} 人"))
            ),
        ),
        (None, _) => check(
            CheckItem::Swarm,
            CheckLevel::Warn,
            "tracker 没报人数".to_owned(),
        ),
    });
    checks.push(match facts.upload_limit {
        Some(limit) if limit.get() < LOW_UPLOAD => check(
            CheckItem::Upload,
            CheckLevel::Warn,
            format!(
                "上传限速 {} KB/s：别人多按你传给他们的多少回传，限得太低下载也会慢",
                limit.get() >> 10
            ),
        ),
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
            trackers: vec![Tracker {
                working: true,
                fails: 0,
                message: String::new(),
            }],
            seeds: Some(12),
            leechers: Some(3),
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
        let mut facts = healthy();
        facts.upnp_port = 0;
        facts.upnp_error = "no router".into();
        let mapping = checks(&facts)
            .into_iter()
            .find(|check| check.item == CheckItem::PortMapping)
            .unwrap();
        assert_eq!(mapping.level, CheckLevel::Warn);
        assert!(mapping.text.contains("no router") && mapping.text.contains("51413"));
        let mut facts = healthy();
        facts.dht_nodes = Some(0);
        assert_eq!(level(&facts, CheckItem::Dht), CheckLevel::Bad);
        // 私有种子不用 DHT，也就不算不通
        facts.private = true;
        assert_eq!(level(&facts, CheckItem::Dht), CheckLevel::Ok);
        let mut facts = healthy();
        facts.trackers = vec![
            Tracker {
                working: false,
                fails: 2,
                message: "timed out".into(),
            };
            2
        ];
        let trackers = checks(&facts)
            .into_iter()
            .find(|check| check.item == CheckItem::Trackers)
            .unwrap();
        assert_eq!(trackers.level, CheckLevel::Bad);
        assert!(trackers.text.contains("timed out"));
        facts.trackers[0].fails = 0;
        assert_eq!(level(&facts, CheckItem::Trackers), CheckLevel::Warn);
        let mut facts = healthy();
        facts.seeds = Some(0);
        facts.leechers = Some(0);
        assert_eq!(level(&facts, CheckItem::Swarm), CheckLevel::Bad);
        facts.leechers = Some(4);
        assert_eq!(level(&facts, CheckItem::Swarm), CheckLevel::Warn);
        let mut facts = healthy();
        facts.incoming = false;
        facts.upload_limit = NonZeroU32::new(20 << 10);
        assert_eq!(level(&facts, CheckItem::Incoming), CheckLevel::Warn);
        assert_eq!(level(&facts, CheckItem::Upload), CheckLevel::Warn);
    }
}
