//! 网页的 Cookie 存储：取出会随请求发往某个地址的 Cookie。各平台的网页引擎只给异步接口，在界面线程上回调；这里等回调
//! 而不阻塞界面线程（wry 的 `cookies_for_url` 在界面线程上嵌套跑事件循环等结果，调用方持着界面状态时会被重入）。

use std::future::Future;

use url::Url;

/// 一条 Cookie。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// 跨站的请求（发起请求的页面与请求的地址不在同一站点）也带它：SameSite=None，或没设 SameSite 而网页引擎按 None
    /// 处理（WebKit 如此；WebView2 按 Lax 处理，不带）
    pub cross_site: bool,
}

/// 网页引擎回给 [`cookies_for`] 的结果。
type Answer = Result<Vec<Cookie>, String>;

/// `webview` 的 Cookie 存储里会随请求发往 `url` 的 Cookie：域、路径、Secure 都合，没过期。要在界面线程上调；返回的
/// future 等引擎在界面线程上回调，不阻塞。引擎报错、没有回调就释放了网页时为原因；引擎一直不回调时一直不完成，时限由
/// 调用方定。
pub fn cookies_for(webview: &wry::WebView, url: &Url) -> impl Future<Output = Answer> + 'static {
    let asked = platform::ask(webview, url);
    async move {
        asked?
            .await
            .map_err(|_| "网页引擎没有回出 Cookie 就释放了网页".to_owned())?
    }
}

/// 域为 `domain`、路径为 `path` 的 Cookie 会不会随请求发往 `url`（RFC 6265 第 5.4 节）：`domain` 以 `.` 开头的对这个域
/// 与它的子域都有效，否则只对这个主机；`secure` 的只发往 https 与本机。
#[cfg(any(target_os = "macos", test))]
fn sent_to(url: &Url, domain: &str, path: &str, secure: bool) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();
    let domain_ok = match domain.strip_prefix('.') {
        Some(parent) => {
            host == parent
                || (matches!(url.host(), Some(url::Host::Domain(_)))
                    && host.ends_with(&domain)
                    && host.len() > domain.len())
        }
        None => host == domain,
    };
    let request = url.path();
    let path_ok = request == path
        || (request.starts_with(path)
            && (path.ends_with('/') || request[path.len()..].starts_with('/')));
    let local = host == "localhost"
        || url.host().is_some_and(|host| match host {
            url::Host::Ipv4(ip) => ip.is_loopback(),
            url::Host::Ipv6(ip) => ip.is_loopback(),
            url::Host::Domain(_) => false,
        });
    let secure_ok = !secure || url.scheme() == "https" || local;
    domain_ok && path_ok && secure_ok
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::Cell;
    use std::ptr::NonNull;
    use std::time::{SystemTime, UNIX_EPOCH};

    use block2::RcBlock;
    use objc2_foundation::{NSArray, NSHTTPCookie};
    use tokio::sync::oneshot;
    use url::Url;
    use wry::WebViewExtMacOS as _;

    use super::{Answer, Cookie, sent_to};

    /// 向 WKWebView 的 Cookie 存储要全部 Cookie，回调里按 `url` 筛（WKHTTPCookieStore 没有按地址取的接口）。
    pub(super) fn ask(
        webview: &wry::WebView,
        url: &Url,
    ) -> Result<oneshot::Receiver<Answer>, String> {
        let (sender, receiver) = oneshot::channel();
        let sender = Cell::new(Some(sender));
        let url = url.clone();
        let handler = RcBlock::new(move |cookies: NonNull<NSArray<NSHTTPCookie>>| {
            // SAFETY: WebKit 交给回调的数组非空、在回调期间有效。
            let cookies = unsafe { cookies.as_ref() };
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0.0, |since| since.as_secs_f64());
            let found = cookies
                .to_vec()
                .iter()
                .filter(|cookie| {
                    let expired = cookie
                        .expiresDate()
                        .is_some_and(|date| date.timeIntervalSince1970() <= now);
                    !expired
                        && sent_to(
                            &url,
                            &cookie.domain().to_string(),
                            &cookie.path().to_string(),
                            cookie.isSecure(),
                        )
                })
                .map(|cookie| Cookie {
                    name: cookie.name().to_string(),
                    value: cookie.value().to_string(),
                    // 没设 SameSite（nil）的，WebKit 跨站也带；lax、strict 不带
                    cross_site: cookie.sameSitePolicy().is_none(),
                })
                .collect();
            if let Some(sender) = sender.take() {
                // 调用方不再等（嗅探页关了）时没有谁要这个结果
                let _caller_gone = sender.send(Ok(found));
            }
        });
        let webview = webview.webview();
        // SAFETY: 在界面线程上调（WKWebView 只能在主线程上用），网页视图此刻有效。
        let configuration = unsafe { webview.configuration() };
        // SAFETY: 同上；配置是这个网页视图的。
        let store = unsafe { configuration.websiteDataStore() };
        // SAFETY: 同上；数据存储是这个网页视图的。
        let cookies = unsafe { store.httpCookieStore() };
        // SAFETY: 在界面线程上调；WebKit 复制并持有回调块直到调用它，回调里只用它自己持有的值。
        unsafe { cookies.getAllCookies(&handler) };
        Ok(receiver)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use tokio::sync::oneshot;
    use url::Url;
    use webkit2gtk::{CookieManagerExt as _, WebViewExt as _, WebsiteDataManagerExt as _};
    use wry::WebViewExtUnix as _;

    use super::{Answer, Cookie};

    /// 向 WebKitGTK 的 Cookie 管理器要发往 `url` 的 Cookie（它按地址筛好）。
    pub(super) fn ask(
        webview: &wry::WebView,
        url: &Url,
    ) -> Result<oneshot::Receiver<Answer>, String> {
        let manager = webview
            .webview()
            .website_data_manager()
            .and_then(|data| data.cookie_manager())
            .ok_or_else(|| "网页没有 Cookie 存储".to_owned())?;
        let (sender, receiver) = oneshot::channel();
        manager.cookies(
            url.as_str(),
            None::<&gtk::gio::Cancellable>,
            move |result| {
                let answer = result
                    .map(|cookies| cookies.into_iter().map(cookie_of).collect())
                    .map_err(|error| format!("网页引擎读不出 Cookie：{error}"));
                // 调用方不再等（嗅探页关了）时没有谁要这个结果
                let _caller_gone = sender.send(answer);
            },
        );
        Ok(receiver)
    }

    fn cookie_of(mut cookie: soup::Cookie) -> Cookie {
        Cookie {
            name: cookie
                .name()
                .map(|name| name.to_string())
                .unwrap_or_default(),
            value: cookie
                .value()
                .map(|value| value.to_string())
                .unwrap_or_default(),
            // libsoup 把没设 SameSite 的记为 None：跨站也带
            cross_site: matches!(cookie.same_site_policy(), soup::SameSitePolicy::None),
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use tokio::sync::oneshot;
    use url::Url;
    use webview2_com::GetCookiesCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE, ICoreWebView2_2, ICoreWebView2Cookie,
        ICoreWebView2CookieList,
    };
    use windows::core::{HSTRING, Interface as _, PWSTR};
    use wry::WebViewExtWindows as _;

    use super::{Answer, Cookie};

    /// 向 WebView2 的 Cookie 管理器要发往 `url` 的 Cookie（它按地址筛好）。
    pub(super) fn ask(
        webview: &wry::WebView,
        url: &Url,
    ) -> Result<oneshot::Receiver<Answer>, String> {
        let core = webview
            .webview()
            .cast::<ICoreWebView2_2>()
            .map_err(|error| format!("网页引擎太旧，取不了 Cookie：{error}"))?;
        // SAFETY: 在界面线程上调（WebView2 只能在建它的线程上用），网页视图此刻有效。
        let manager = unsafe { core.CookieManager() }
            .map_err(|error| format!("网页没有 Cookie 存储：{error}"))?;
        let (sender, receiver) = oneshot::channel();
        let handler = GetCookiesCompletedHandler::create(Box::new(move |status, list| {
            let answer = status
                .and_then(|()| list.map_or_else(|| Ok(Vec::new()), |list| cookies_of(&list)))
                .map_err(|error| format!("网页引擎读不出 Cookie：{error}"));
            // 调用方不再等（嗅探页关了）时没有谁要这个结果
            let _caller_gone = sender.send(answer);
            Ok(())
        }));
        let uri = HSTRING::from(url.as_str());
        // SAFETY: 在界面线程上调；`uri` 活到调用返回，回调由 WebView2 持有到调用它。
        unsafe { manager.GetCookies(&uri, &handler) }
            .map_err(|error| format!("网页引擎读不出 Cookie：{error}"))?;
        Ok(receiver)
    }

    fn cookies_of(list: &ICoreWebView2CookieList) -> windows::core::Result<Vec<Cookie>> {
        let mut count = 0;
        // SAFETY: 列表由 WebView2 交给回调，回调期间有效。
        unsafe { list.Count(&mut count) }?;
        (0..count)
            .map(|index| {
                // SAFETY: 同上；下标小于它报的个数。
                let cookie = unsafe { list.GetValueAtIndex(index) }?;
                cookie_of(&cookie)
            })
            .collect()
    }

    fn cookie_of(cookie: &ICoreWebView2Cookie) -> windows::core::Result<Cookie> {
        let mut name = PWSTR::null();
        // SAFETY: Cookie 由 WebView2 交出，有效；写出的字符串归调用方，由 take_pwstr 释放。
        unsafe { cookie.Name(&mut name) }?;
        let mut value = PWSTR::null();
        // SAFETY: 同上。
        unsafe { cookie.Value(&mut value) }?;
        let mut same_site = COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE;
        // SAFETY: 同上；写出的是枚举值。
        unsafe { cookie.SameSite(&mut same_site) }?;
        Ok(Cookie {
            name: webview2_com::take_pwstr(name),
            value: webview2_com::take_pwstr(value),
            cross_site: same_site == COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn cookies_match_domain_path_and_scheme() {
        let page = url("https://cdn.video.example/live/a/index.m3u8");
        // 上级域的 Cookie 对子域有效，只对主机的不对别的主机
        assert!(sent_to(&page, ".video.example", "/", true));
        assert!(sent_to(&page, "cdn.video.example", "/live", false));
        assert!(!sent_to(&page, "video.example", "/", false));
        assert!(!sent_to(&page, ".ideo.example", "/", false));
        // 路径按段匹配
        assert!(sent_to(&page, ".video.example", "/live/", false));
        assert!(!sent_to(&page, ".video.example", "/liv", false));
        assert!(!sent_to(&page, ".video.example", "/live/b", false));
        // Secure 的不发往 http，本机除外；IP 地址不按域匹配
        assert!(!sent_to(
            &url("http://cdn.video.example/"),
            ".video.example",
            "/",
            true
        ));
        assert!(sent_to(
            &url("http://localhost:8080/a"),
            "localhost",
            "/",
            true
        ));
        assert!(!sent_to(&url("http://1.2.3.4/"), ".3.4", "/", false));
    }
}
