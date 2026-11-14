//! 请求经哪里走：照设置里的代理（[`hs_m3u8_app::Settings::proxy`]），跟随系统时照系统代理设置读，与任务实际走的
//! 相同；嗅探页的网页照同一个设置（[`web_proxy`]）。读系统代理要调操作系统的接口，在后台调。

use hs_m3u8_core::{Proxy, ProxySource, SystemProxy, Url, bare_url};
use hs_m3u8_webview::WebProxy;

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
            Ok(Proxy::Direct) => Route::Direct("设置为直连"),
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

/// 嗅探页的网页照设置里的代理 `proxy` 走时用的网页代理；网页引擎用不了它时为原因，网页照系统代理。
pub fn web_proxy(proxy: &Proxy) -> Result<WebProxy, String> {
    let web = match proxy {
        Proxy::System => WebProxy::System,
        Proxy::Direct => WebProxy::Direct,
        Proxy::Url(url) => {
            if !url.username().is_empty() || url.password().is_some() {
                return Err("网页用不了带用户名、密码的代理".to_owned());
            }
            let host = url
                .host_str()
                .ok_or_else(|| "代理地址没有主机".to_owned())?
                .to_owned();
            let port = url
                .port_or_known_default()
                .ok_or_else(|| "网页用的代理地址要写端口".to_owned())?;
            match url.scheme() {
                "http" => WebProxy::Http { host, port },
                "socks5" | "socks5h" => WebProxy::Socks5 { host, port },
                other => return Err(format!("网页只能用 http 或 socks5 代理，用不了 {other}")),
            }
        }
    };
    hs_m3u8_webview::supported(&web)?;
    Ok(web)
}
