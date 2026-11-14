//! 后台探测找到的流：一次一条，读出网页引擎里发往它的 Cookie、配好请求头、探测。

use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{BackgroundExecutor, Context};
use hs_m3u8_app::{Failure, Service};
use hs_m3u8_core::{Source, Url};
use hs_m3u8_webview::cookies::{Cookie, cookies_for};

use super::found::Check;
use super::{SniffView, request};
use crate::new_task;
use crate::wait::within;

/// 等网页引擎回出 Cookie 最多多久：引擎卡住时不让探测一直挂着。
const COOKIE_TIMEOUT: Duration = Duration::from_secs(5);
/// 探测一条流最多多久，到时取消（连同核心在途的请求与重试）：源站不回应时不让后面的一直排着。核心默认连接超时 15
/// 秒、第一次重试前等约 0.5 秒，30 秒容得下一次连接超时后再试一次。
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// 要探测的流与它的来历（见 [`request::Found`]）。
struct Request {
    stream: Url,
    frame: Option<Url>,
    top: Option<Url>,
    agent: String,
}

impl SniffView {
    /// 没在探测时，探测下一条等着的流。网页建不出来时不探测（列表里写着原因）。
    pub(super) fn check_next(&mut self, cx: &mut Context<Self>) {
        if self.checking.is_some() {
            return;
        }
        let Some(webview) = self.webview().cloned() else {
            return;
        };
        let Some(found) = self
            .found
            .iter_mut()
            .find(|found| matches!(found.check, Check::Waiting))
        else {
            return;
        };
        found.check = Check::Running;
        let cookies = cookies_for(webview.read(cx).raw(), &found.url);
        let request = Request {
            stream: found.url.clone(),
            frame: found.frame.clone(),
            top: found.top.clone(),
            agent: found.agent.clone(),
        };
        let service = self.service.clone();
        self.checking = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let (check, skipped) = run(&service, cookies, &request, &executor).await;
            // 关了时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                this.checking = None;
                // 换页时列表清空、探测随之取消，探测中的也不换地址：这条一定还在
                let entry = this
                    .found
                    .iter_mut()
                    .find(|entry| entry.url == request.stream)
                    .expect("探测中的流在探测完之前一直在列表里");
                entry.check = check;
                entry.skipped = skipped;
                this.check_next(cx);
                cx.notify();
            });
        }));
    }

    /// 没成的流重新排队探测（重新读 Cookie）：`url` 为那一条，None 为全部没成的。
    pub(super) fn retry(&mut self, url: Option<&Url>, cx: &mut Context<Self>) {
        for found in &mut self.found {
            if matches!(found.check, Check::Failed { .. })
                && url.is_none_or(|url| *url == found.url)
            {
                found.check = Check::Waiting;
            }
        }
        self.check_next(cx);
        cx.notify();
    }
}

/// 读出 Cookie、配好请求头、探测；返回结果与没带上的 Cookie 个数。
async fn run(
    service: &Rc<Service>,
    cookies: impl Future<Output = Result<Vec<Cookie>, String>>,
    request: &Request,
    executor: &BackgroundExecutor,
) -> (Check, usize) {
    let cookies = within(std::pin::pin!(cookies), executor.timer(COOKIE_TIMEOUT))
        .await
        .unwrap_or_else(|| {
            Err(format!(
                "网页引擎 {} 秒没有回出 Cookie",
                COOKIE_TIMEOUT.as_secs()
            ))
        });
    let made = cookies.and_then(|cookies| {
        request::headers_for(&request::Found {
            stream: &request.stream,
            frame: request.frame.as_ref(),
            agent: &request.agent,
            top: request.top.as_ref(),
            cookies,
        })
    });
    let made = match made {
        Ok(made) => made,
        Err(reason) => {
            return (
                Check::Failed {
                    reason,
                    headers: None,
                },
                0,
            );
        }
    };
    let mut source = Source::new(request.stream.clone());
    source.http.headers.clone_from(&made.headers);
    // 超时即释放句柄、取消探测
    let probing = std::pin::pin!(service.probe(source));
    let failed = |reason| Check::Failed {
        reason,
        headers: Some(made.headers.clone()),
    };
    let check = match within(probing, executor.timer(PROBE_TIMEOUT)).await {
        Some(Ok(Ok(probe))) => Check::Ready {
            summary: new_task::describe(&probe).join(" · "),
            probe: Box::new(probe),
            headers: made.headers.clone(),
        },
        Some(Ok(Err(error))) => failed(Failure::of(&error).message),
        Some(Err(join)) => failed(format!("探测中途出错：{join}")),
        None => failed(format!(
            "探测 {} 秒没有结果，已停下",
            PROBE_TIMEOUT.as_secs()
        )),
    };
    (check, made.skipped)
}
