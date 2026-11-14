//! 网页的请求经哪个代理。各平台的网页引擎能做到的不同（[`supported`]）：
//!
//! - macOS：14 起能给网页设 HTTP（CONNECT）或 SOCKS5 代理，设在网页的数据存储上（进程里的网页共用一个）；不能单独设为
//!   直连。
//! - Linux：WebKitGTK 的跟随系统、直连、自定义代理都能设，设在网页的数据管理器上。
//! - Windows：WebView2 的 HTTP 或 SOCKS5 代理在建网页时作为启动参数传入；不能单独设为直连。
//!
//! 都不支持带用户名、密码的代理（引擎只收主机与端口）。设代理要在网页开始加载之前（见 [`crate::WebView::build`]）。

/// 网页的请求经的代理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebProxy {
    /// 照系统代理设置
    System,
    /// 不经代理
    Direct,
    /// 经 HTTP 代理（用 CONNECT 建隧道）
    Http {
        host: String,
        port: u16,
    },
    Socks5 {
        host: String,
        port: u16,
    },
}

/// 这个平台的网页能不能用 `proxy`；不能时为原因。
pub fn supported(proxy: &WebProxy) -> Result<(), String> {
    match proxy {
        WebProxy::System => Ok(()),
        #[cfg(target_os = "linux")]
        WebProxy::Direct | WebProxy::Http { .. } | WebProxy::Socks5 { .. } => Ok(()),
        #[cfg(not(target_os = "linux"))]
        WebProxy::Direct => Err("这个系统上网页不能单独设为直连".to_owned()),
        #[cfg(target_os = "macos")]
        WebProxy::Http { .. } | WebProxy::Socks5 { .. } => {
            if macos::custom_proxy_available() {
                Ok(())
            } else {
                Err("macOS 14 起网页才能单独设代理".to_owned())
            }
        }
        #[cfg(target_os = "windows")]
        WebProxy::Http { .. } | WebProxy::Socks5 { .. } => Ok(()),
    }
}

/// 建网页之前：macOS、Windows 上的 HTTP、SOCKS5 代理经 wry 设。
#[cfg(not(target_os = "linux"))]
pub(crate) fn before_build<'a>(
    builder: wry::WebViewBuilder<'a>,
    proxy: &WebProxy,
) -> wry::WebViewBuilder<'a> {
    let endpoint = |host: &String, port: &u16| wry::ProxyEndpoint {
        host: host.clone(),
        port: port.to_string(),
    };
    match proxy {
        WebProxy::System | WebProxy::Direct => builder,
        WebProxy::Http { host, port } => {
            builder.with_proxy_config(wry::ProxyConfig::Http(endpoint(host, port)))
        }
        WebProxy::Socks5 { host, port } => {
            builder.with_proxy_config(wry::ProxyConfig::Socks5(endpoint(host, port)))
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn before_build<'a>(
    builder: wry::WebViewBuilder<'a>,
    _: &WebProxy,
) -> wry::WebViewBuilder<'a> {
    builder
}

/// 建好网页、开始加载之前：macOS 上跟随系统时清掉数据存储上之前设的代理（数据存储在进程里共用）。
#[cfg(target_os = "macos")]
pub(crate) fn after_build(webview: &wry::WebView, proxy: &WebProxy) {
    if *proxy == WebProxy::System && macos::custom_proxy_available() {
        macos::clear(webview);
    }
}

/// 建好网页、开始加载之前：Linux 上照 `proxy` 设网页的数据管理器（各网页共用默认的上下文）。
#[cfg(target_os = "linux")]
pub(crate) fn after_build(webview: &wry::WebView, proxy: &WebProxy) {
    use webkit2gtk::{
        NetworkProxyMode, NetworkProxySettings, WebContextExt as _, WebViewExt as _,
        WebsiteDataManagerExt as _,
    };
    use wry::WebViewExtUnix as _;

    let manager = webview
        .webview()
        .context()
        .and_then(|context| context.website_data_manager())
        .expect("WebKitGTK 的网页总有上下文与数据管理器");
    let custom = |uri: String| {
        let mut settings = NetworkProxySettings::new(Some(&uri), &[]);
        manager.set_network_proxy_settings(NetworkProxyMode::Custom, Some(&mut settings));
    };
    match proxy {
        WebProxy::System => manager.set_network_proxy_settings(NetworkProxyMode::Default, None),
        WebProxy::Direct => manager.set_network_proxy_settings(NetworkProxyMode::NoProxy, None),
        WebProxy::Http { host, port } => custom(format!("http://{host}:{port}")),
        WebProxy::Socks5 { host, port } => custom(format!("socks5://{host}:{port}")),
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn after_build(_: &wry::WebView, _: &WebProxy) {}

#[cfg(target_os = "macos")]
mod macos {
    use objc2_foundation::{
        NSArray, NSObjectNSKeyValueCoding as _, NSOperatingSystemVersion, NSProcessInfo, ns_string,
    };
    use objc2_web_kit::WKWebView;
    use wry::WebViewExtMacOS as _;

    /// 数据存储的 `proxyConfigurations` 从 macOS 14 起才有；更早的系统上设它会因认不出而中止。
    pub(super) fn custom_proxy_available() -> bool {
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: 14,
            minorVersion: 0,
            patchVersion: 0,
        })
    }

    /// 清掉网页的数据存储上设的代理：空数组即照系统代理设置。
    pub(super) fn clear(webview: &wry::WebView) {
        let view: &WKWebView = &webview.webview();
        // SAFETY: 在界面线程上调（WKWebView 只能在主线程上用），网页此刻有效；配置与数据存储随网页存在。
        let store = unsafe { view.configuration() };
        // SAFETY: 同上。
        let store = unsafe { store.websiteDataStore() };
        let empty = NSArray::<objc2::runtime::NSObject>::new();
        // SAFETY: 调用方已确认系统为 macOS 14 起，数据存储有 proxyConfigurations；值为数组，类型与它相符。
        unsafe { store.setValue_forKey(Some(&empty), ns_string!("proxyConfigurations")) };
    }
}
