//! 任务的请求经哪里走（显示用）：照设置里的代理（[`hs_m3u8_app::Settings::proxy`]），跟随系统时照系统代理设置读，
//! 与任务实际走的相同。读系统代理要调操作系统的接口，在后台调。嗅探页的网页见 [`crate::web_route`]。

use hs_m3u8_core::{Proxy, ProxySource, SystemProxy, Url, bare_url};

/// 访问某个地址时请求经哪里走。
pub enum Route {
    /// 经这个代理（已去掉凭据的写法）与它来自哪里
    Proxy(String, &'static str),
    /// 直连，值为原因
    Direct(&'static str),
    /// 设置或系统代理读不出、用不了，值为原因；任务开始时同样会失败
    Broken(String),
}

impl Route {
    /// 访问 `url` 时的；`proxy` 为设置里的代理，读不出设置时为原因。
    pub fn of(url: &Url, proxy: Result<Proxy, impl ToString>) -> Route {
        match proxy {
            Err(error) => Route::Broken(error.to_string()),
            Ok(Proxy::Direct) => Route::Direct("按设置"),
            Ok(Proxy::Url(proxy)) => Route::Proxy(bare_url(&proxy), "设置"),
            Ok(Proxy::System) => match hs_m3u8_core::system_proxy() {
                Ok(Some(system)) => match system.proxy_for(url) {
                    Some(proxy) => Route::Proxy(bare_url(proxy), source_name(&system)),
                    None => Route::Direct("这个地址不经代理"),
                },
                Ok(None) => Route::Direct("系统没设代理"),
                Err(error) => Route::Broken(error.to_string()),
            },
        }
    }

    /// 跟随系统时一般的网址（https）经哪里走：系统代理设置里给 https 的代理，没有时给 http 的。
    pub fn of_system() -> Route {
        match hs_m3u8_core::system_proxy() {
            Ok(Some(system)) => match system.https.as_ref().or(system.http.as_ref()) {
                Some(proxy) => Route::Proxy(bare_url(proxy), source_name(&system)),
                None => Route::Direct("系统没设代理"),
            },
            Ok(None) => Route::Direct("系统没设代理"),
            Err(error) => Route::Broken(error.to_string()),
        }
    }
}

/// 系统代理设置从哪里读到的说法。
fn source_name(system: &SystemProxy) -> &'static str {
    match system.source {
        ProxySource::Environment => "环境变量",
        ProxySource::MacOs => "系统设置",
        ProxySource::Windows => "Internet 选项",
    }
}
