//! 探测嗅探到的流时带的请求头：照网页里的播放器请求它时浏览器会带的给。

use hs_m3u8_core::{Url, check_option_header};
use hs_m3u8_webview::cookies::Cookie;

/// 探测时带的请求头与没带上的 Cookie 个数。
#[derive(Debug, PartialEq, Eq)]
pub struct Headers {
    pub headers: Vec<(String, String)>,
    /// 没有名字、核心收不了（只收 `名=值`）的 Cookie 个数
    pub nameless: usize,
}

/// 探测流 `stream` 要带的请求头，都按核心的规则校验过：
///
/// - Referer：流所在框架的 `origin`（只到主机与端口，浏览器跨站时的默认做法；核心会把它发给分片与 key 所在的每个
///   主机，完整的页面地址里常有会话参数）；`origin` 不是 http(s)，或从 https 页面请求 http 的流时不带。
/// - User-Agent：网页的 `agent`，为空时不带。
/// - Cookie：网页引擎里会发往这个流的 `cookies`。顶层页面 `top` 与流同一站点（协议相同、可注册域名相同）时全带；
///   否则只带跨站也带的（[`Cookie::cross_site`]），网页里的播放器跨站请求时浏览器同样只带这些。
pub fn headers_for(
    stream: &Url,
    origin: Option<&Url>,
    agent: &str,
    top: Option<&Url>,
    cookies: Vec<Cookie>,
) -> Result<Headers, String> {
    let mut headers = Vec::new();
    if let Some(origin) = origin
        && !(origin.scheme() == "https" && stream.scheme() == "http")
    {
        headers.push((
            "Referer".to_owned(),
            format!("{}/", origin.origin().ascii_serialization()),
        ));
    }
    if !agent.is_empty() {
        headers.push(("User-Agent".to_owned(), agent.to_owned()));
    }
    let same_site = top.is_some_and(|top| same_site(top, stream));
    let (named, nameless): (Vec<Cookie>, Vec<Cookie>) = cookies
        .into_iter()
        .filter(|cookie| same_site || cookie.cross_site)
        .partition(|cookie| !cookie.name.is_empty());
    if !named.is_empty() {
        let pairs: Vec<String> = named
            .iter()
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect();
        headers.push(("Cookie".to_owned(), pairs.join("; ")));
    }
    for (name, value) in &headers {
        check_option_header(name, value)?;
    }
    Ok(Headers {
        headers,
        nameless: nameless.len(),
    })
}

/// `a` 与 `b` 同一站点：协议相同、可注册域名（按公共后缀列表）相同；IP 地址与列表里认不出的主机按整个主机比。
fn same_site(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme() && site(a).is_some_and(|site_a| Some(site_a) == site(b))
}

fn site(url: &Url) -> Option<String> {
    let host = url.host()?;
    let text = host.to_string().to_ascii_lowercase();
    Some(match host {
        url::Host::Domain(_) => psl::domain_str(&text).map_or_else(|| text.clone(), str::to_owned),
        url::Host::Ipv4(_) | url::Host::Ipv6(_) => text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn cookie(name: &str, cross_site: bool) -> Cookie {
        Cookie {
            name: name.to_owned(),
            value: "1".to_owned(),
            cross_site,
        }
    }

    #[test]
    fn referer_is_the_frame_origin() {
        let stream = url("https://cdn.video.example/a/index.m3u8");
        let origin = url("https://www.video.example");
        let made = headers_for(&stream, Some(&origin), "UA", None, Vec::new()).unwrap();
        assert_eq!(
            made.headers,
            [
                (
                    "Referer".to_owned(),
                    "https://www.video.example/".to_owned()
                ),
                ("User-Agent".to_owned(), "UA".to_owned()),
            ]
        );
        // 从 https 页面请求 http 的流不带
        let plain = url("http://cdn.video.example/a/index.m3u8");
        let made = headers_for(&plain, Some(&origin), "", None, Vec::new()).unwrap();
        assert_eq!(made.headers, []);
    }

    #[test]
    fn cross_site_streams_get_only_cross_site_cookies() {
        let stream = url("https://cdn.video.example/a/index.m3u8");
        let cookies = || vec![cookie("sid", false), cookie("cdn", true), cookie("", true)];
        // 同一站点（子域不同）：全带，没名字的不带并计数
        let top = url("https://www.video.example/watch/3");
        let made = headers_for(&stream, None, "", Some(&top), cookies()).unwrap();
        assert_eq!(
            made.headers,
            [("Cookie".to_owned(), "sid=1; cdn=1".to_owned())]
        );
        assert_eq!(made.nameless, 1);
        // 别的站点：只带跨站也带的；可注册域名按公共后缀列表算（example.co.uk，不是 co.uk）
        let other = url("https://player.example.co.uk/");
        let made = headers_for(&stream, None, "", Some(&other), cookies()).unwrap();
        assert_eq!(made.headers, [("Cookie".to_owned(), "cdn=1".to_owned())]);
        assert!(same_site(&url("https://a.example.co.uk/"), &other));
        assert!(!same_site(&url("https://a.other.co.uk/"), &other));
        // 协议不同不算同一站点
        assert!(!same_site(&url("http://www.video.example/"), &stream));
    }
}
