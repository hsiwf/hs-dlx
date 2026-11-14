//! 嗅探页的网页经哪里出去。网页引擎的代理在进程里共用（macOS 设在共用的数据存储上，WebView2 同时开着的网页要用
//! 同样的启动参数），所以按「此刻有没有网页开着」统一决定：没有网页开着时，下一个网页按设置定；有网页开着时，新建
//! 的照用它们的。
//!
//! - 跟随系统：交给网页引擎自己照系统代理走（含自动代理配置 PAC 与自动发现）。
//! - 直连、指定代理：网页指向本机的中转（hs-m3u8-relay），由它直连或经上游代理（可带口令）出去。中转在第一个网页建
//!   出时开，最后一个网页关掉时停。网页开着时设置在直连与指定代理之间改了，之后的连接立即照新的走（见
//!   [`WebRoute::setting_changed`]）；与跟随系统之间改的，网页都关掉后才换。
//!
//! 平台不能单独设代理而设置是直连或指定代理时，不建网页（照系统代理走就违背了设置）。

use std::net::Ipv4Addr;
use std::rc::{Rc, Weak};
use std::sync::{Arc, RwLock};

use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};
use hs_m3u8_core::Proxy;
use hs_m3u8_relay::{Relay, Target, Upstream};
use hs_m3u8_webview::WebProxy;
use tokio::sync::mpsc;

/// 中转报来、还没分发的连不过去的最多积多少条：网页能让中转连不过去任意多次，再多的丢掉。
const FAILURES: usize = 64;

/// 中转连不过去：目标与原因（网页引擎拿到中转的 502 多半不报原因，嗅探页据此告诉用户）。
pub struct RelayFailure {
    pub target: Target,
    pub reason: String,
}

/// 开着的网页共用的出口，网页开着期间持有；最后一个网页关掉即释放（中转随之停下）。
pub struct Lease {
    pub proxy: WebProxy,
    /// 定出口时设置里是跟随系统
    system: bool,
    /// 中转与它此刻经哪里出去；跟随系统时为 None
    relay: Option<(Relay, Arc<RwLock<Upstream>>)>,
}

impl Lease {
    /// 网页此刻经哪里出去（显示用）：跟随系统、直连或经哪个代理（已去掉凭据的写法）。
    pub fn egress(&self) -> String {
        match &self.relay {
            None => "跟随系统".to_owned(),
            Some((_, current)) => match &*current
                .read()
                .expect("持锁时只赋值与克隆，不会 panic 留下中毒的锁")
            {
                Upstream::Direct => "直连".to_owned(),
                Upstream::Proxy(url) => format!("经 {}", hs_m3u8_core::bare_url(url)),
            },
        }
    }
}

/// 进程里的出口：开着的网页照的出口，与把中转报来的连不过去发给嗅探页。
pub struct WebRoute {
    /// 开着的网页照的；都关了时升级不成
    open: Weak<Lease>,
    /// 中转报来的连不过去，由 `_dispatching` 在界面线程上发给订阅的嗅探页
    failed: mpsc::Sender<RelayFailure>,
    _dispatching: Task<()>,
}

impl EventEmitter<RelayFailure> for WebRoute {}

struct Installed(Entity<WebRoute>);

impl Global for Installed {}

impl WebRoute {
    /// 进程里的出口；第一次调时建。
    pub fn get(cx: &mut App) -> Entity<WebRoute> {
        if let Some(Installed(route)) = cx.try_global::<Installed>() {
            return route.clone();
        }
        let route = cx.new(|cx: &mut Context<WebRoute>| {
            let (failed, mut receiving) = mpsc::channel(FAILURES);
            let dispatching = cx.spawn(async move |this, cx| {
                while let Some(failure) = receiving.recv().await {
                    if this.update(cx, |_, cx| cx.emit(failure)).is_err() {
                        break;
                    }
                }
            });
            WebRoute {
                open: Weak::new(),
                failed,
                _dispatching: dispatching,
            }
        });
        cx.set_global(Installed(route.clone()));
        route
    }

    /// 给一个要建的网页：网页该设的代理，网页开着期间持有。`setting` 为此刻设置里的代理。有网页开着时照它们的；
    /// 设置与它们照的在跟随系统与另两种之间不同时，附上说明。平台不能单独设代理而设置要求、开不了中转时为原因。
    pub fn acquire(&mut self, setting: &Proxy) -> Result<(Rc<Lease>, Option<String>), String> {
        if let Some(open) = self.open.upgrade() {
            let note = (open.system != (*setting == Proxy::System))
                .then(|| "代理设置改过了：嗅探页的网页都关掉、再打开后才照新的走".to_owned());
            return Ok((open, note));
        }
        let lease = match upstream_of(setting) {
            None => Lease {
                proxy: WebProxy::System,
                system: true,
                relay: None,
            },
            Some(upstream) => {
                hs_m3u8_webview::custom_proxy_supported().map_err(|reason| {
                    format!("{reason}；设置里是直连或指定代理，改成跟随系统才能打开网页")
                })?;
                let (relay, current) = self.start_relay(upstream)?;
                Lease {
                    proxy: WebProxy::Http {
                        host: Ipv4Addr::LOCALHOST.to_string(),
                        port: relay.port(),
                    },
                    system: false,
                    relay: Some((relay, current)),
                }
            }
        };
        let lease = Rc::new(lease);
        self.open = Rc::downgrade(&lease);
        Ok((lease, None))
    }

    /// 设置里的代理改成了 `setting`（已存进）：开着的网页经中转、新设置也是直连或指定代理时，之后的连接立即照新的
    /// 走；否则开着的网页照旧，都关掉后再按新设置定。
    pub fn setting_changed(&mut self, setting: &Proxy) {
        if let Some(open) = self.open.upgrade()
            && let Some((_, current)) = &open.relay
            && let Some(upstream) = upstream_of(setting)
        {
            *current
                .write()
                .expect("持锁时只赋值与克隆，不会 panic 留下中毒的锁") = upstream;
        }
    }

    /// 开一个经 `upstream` 出去的中转，与能改它出口的句柄。
    fn start_relay(&self, upstream: Upstream) -> Result<(Relay, Arc<RwLock<Upstream>>), String> {
        let current = Arc::new(RwLock::new(upstream));
        let route: hs_m3u8_relay::Route = {
            let current = current.clone();
            Arc::new(move |_: &Target| {
                Ok(current
                    .read()
                    .expect("持锁时只赋值与克隆，不会 panic 留下中毒的锁")
                    .clone())
            })
        };
        let failed = self.failed.clone();
        let report: hs_m3u8_relay::Report = Arc::new(move |target, reason| {
            // 积满了（网页一下连不过去很多次）时丢掉
            let _dropped = failed.try_send(RelayFailure {
                target: target.clone(),
                reason: reason.to_owned(),
            });
        });
        Ok((Relay::start(route, report)?, current))
    }
}

/// 设置要中转经哪里出去；跟随系统（交给网页引擎）时为 None。
fn upstream_of(setting: &Proxy) -> Option<Upstream> {
    match setting {
        Proxy::System => None,
        Proxy::Direct => Some(Upstream::Direct),
        Proxy::Url(url) => Some(Upstream::Proxy(url.clone())),
    }
}
