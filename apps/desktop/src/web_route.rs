//! 嗅探页的网页经哪里出去。网页引擎的代理在进程里共用（macOS 设在共用的数据存储上，WebView2 同时开着的网页要用
//! 同样的启动参数），所以按「此刻有没有网页开着」统一决定：没有网页开着时，下一个网页按设置定；有网页开着时，新建
//! 的照用它们的。
//!
//! - 跟随系统：交给网页引擎自己照系统代理走（含自动代理配置 PAC 与自动发现）。
//! - 直连、指定代理：网页指向本机的中转（hs-m3u8-relay），由它直连或经上游代理（可带口令）出去。中转整个进程只开
//!   一个，第一次要用时开，留到应用退出；有网页开着时设置在直连与指定代理之间改了，之后的连接立即照新的走。
//!
//! 读不出设置、或平台不能单独设代理而设置是直连或指定代理时，不建网页（照系统代理走就违背了设置）。

use std::net::Ipv4Addr;
use std::rc::{Rc, Weak};
use std::sync::{Arc, RwLock};

use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};
use hs_m3u8_app::Service;
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

/// 开着的网页共用的出口；最后一个网页关掉即释放（见 [`WebRoute::acquire`]）。
pub struct Lease {
    pub proxy: WebProxy,
    /// 网页照的设置（直连与指定代理之间的改动不用重开）
    setting: Proxy,
}

/// 进程里的出口：中转（要用时才开）与开着的网页照的出口。
pub struct WebRoute {
    /// 本机的中转与它此刻经哪里出去；还没用过时为 None
    relay: Option<(Relay, Arc<RwLock<Upstream>>)>,
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
                relay: None,
                open: Weak::new(),
                failed,
                _dispatching: dispatching,
            }
        });
        cx.set_global(Installed(route.clone()));
        route
    }

    /// 给一个要建的网页：网页该设的代理，网页开着期间持有。有网页开着时照它们的（直连与指定代理之间改了时随即改
    /// 中转的出口）；设置改到了要换引擎代理的方式（跟随系统与另两种之间）时，为照旧的并附说明。读不出设置、平台
    /// 不能单独设代理而设置要求、开不了中转时为原因。
    pub fn acquire(&mut self, service: &Service) -> Result<(Rc<Lease>, Option<String>), String> {
        let setting = service
            .settings()
            .map_err(|error| format!("读不出设置：{error}"))?
            .proxy;
        if let Some(open) = self.open.upgrade() {
            let note = match (&open.setting, &setting) {
                (Proxy::System, Proxy::System) => None,
                (Proxy::System, _) | (_, Proxy::System) => {
                    Some("代理设置改过了：嗅探页的网页都关掉、再打开后才照新的走".to_owned())
                }
                (_, _) => {
                    self.relay_to(&setting)?;
                    None
                }
            };
            return Ok((open, note));
        }
        let proxy = match &setting {
            Proxy::System => WebProxy::System,
            Proxy::Direct | Proxy::Url(_) => {
                hs_m3u8_webview::custom_proxy_supported().map_err(|reason| {
                    format!("{reason}；设置里是直连或指定代理，改成跟随系统才能打开网页")
                })?;
                self.relay_to(&setting)?
            }
        };
        let lease = Rc::new(Lease { proxy, setting });
        self.open = Rc::downgrade(&lease);
        Ok((lease, None))
    }

    /// 让中转照 `setting`（直连或指定代理）出去，没开时开；返回指向它的网页代理。
    fn relay_to(&mut self, setting: &Proxy) -> Result<WebProxy, String> {
        let upstream = match setting {
            Proxy::Direct => Upstream::Direct,
            Proxy::Url(url) => Upstream::Proxy(url.clone()),
            Proxy::System => unreachable!("跟随系统交给网页引擎，不经中转"),
        };
        let port = match &self.relay {
            Some((relay, current)) => {
                *current
                    .write()
                    .expect("持锁时只赋值与克隆，不会 panic 留下中毒的锁") = upstream;
                relay.port()
            }
            None => {
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
                let relay = Relay::start(route, report)?;
                let port = relay.port();
                self.relay = Some((relay, current));
                port
            }
        };
        Ok(WebProxy::Http {
            host: Ipv4Addr::LOCALHOST.to_string(),
            port,
        })
    }
}
