//! 网页的请求经哪个代理：照系统代理设置，或经一个 HTTP 代理（应用在本机开的中转，见 hs-m3u8-relay）。各平台的
//! 网页引擎能不能设（[`custom_proxy_supported`]）：
//!
//! - macOS：14 起能设，设在网页的数据存储上（进程里的网页共用一个，跟随系统时清掉之前设的）。
//! - Linux：能设，设在 WebKitGTK 的数据管理器上（各网页共用默认的上下文）。
//! - Windows：能设，建网页时作为 WebView2 的启动参数传入。
//!
//! 引擎只收主机与端口，不收口令。设代理要在网页开始加载之前（见 [`crate::WebView::build`]）。

/// 网页的请求经的代理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebProxy {
    /// 照系统代理设置
    System,
    /// 经这个 HTTP 代理（用 CONNECT 建隧道）
    Http { host: String, port: u16 },
}

/// 这个平台的网页能不能设 [`WebProxy::Http`]；不能时为原因。
pub fn custom_proxy_supported() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if !macos::custom_proxy_available() {
        return Err("macOS 14 起网页才能单独设代理".to_owned());
    }
    Ok(())
}

/// `proxy` 在这个平台上设不设得了；不能时为原因。
pub(crate) fn supported(proxy: &WebProxy) -> Result<(), String> {
    match proxy {
        WebProxy::System => Ok(()),
        WebProxy::Http { .. } => custom_proxy_supported(),
    }
}

/// 建网页之前：macOS、Windows 上的 HTTP 代理经 wry 设。
#[cfg(not(target_os = "linux"))]
pub(crate) fn before_build<'a>(
    builder: wry::WebViewBuilder<'a>,
    proxy: &WebProxy,
) -> wry::WebViewBuilder<'a> {
    match proxy {
        WebProxy::System => builder,
        WebProxy::Http { host, port } => {
            builder.with_proxy_config(wry::ProxyConfig::Http(wry::ProxyEndpoint {
                host: host.clone(),
                port: port.to_string(),
            }))
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
    match proxy {
        WebProxy::System => manager.set_network_proxy_settings(NetworkProxyMode::Default, None),
        WebProxy::Http { host, port } => {
            let uri = format!("http://{host}:{port}");
            let mut settings = NetworkProxySettings::new(Some(&uri), &[]);
            manager.set_network_proxy_settings(NetworkProxyMode::Custom, Some(&mut settings));
        }
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
