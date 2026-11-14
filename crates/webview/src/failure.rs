//! wry 不报的网页故障：顶层导航失败、网页进程退出。wry 的页面加载事件不分成败（打不开时 WebView2 显示自带的错误页，
//! 照样报开始与完成），这里直接接各平台网页引擎的事件。

use std::rc::Rc;

/// 网页出了故障。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// 顶层导航没成（连不上、证书不对、地址不对之类）：网页引擎给的原因。macOS 上 wry 不给这个事件，打不开时只是
    /// 一直不开始显示
    LoadFailed(String),
    /// 网页进程退出或卡死了，刷新才回来
    ProcessGone,
}

/// 收到故障时调的。
pub(crate) type Report = Rc<dyn Fn(Failure)>;

/// 建网页之前：macOS 上网页进程退出只能在建之前登记。
#[cfg(target_os = "macos")]
pub(crate) fn before_build<'a>(
    builder: wry::WebViewBuilder<'a>,
    report: &Report,
) -> wry::WebViewBuilder<'a> {
    use wry::WebViewBuilderExtDarwin as _;
    let report = report.clone();
    builder.with_on_web_content_process_terminate_handler(move || report(Failure::ProcessGone))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn before_build<'a>(
    builder: wry::WebViewBuilder<'a>,
    _: &Report,
) -> wry::WebViewBuilder<'a> {
    builder
}

/// 建好网页之后：WebView2 接导航完成（看是否成功）与进程故障。
#[cfg(target_os = "windows")]
pub(crate) fn after_build(webview: &wry::WebView, report: &Report) -> wry::Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PROCESS_FAILED_KIND, COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE,
        COREWEBVIEW2_WEB_ERROR_STATUS, COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED,
        ICoreWebView2NavigationCompletedEventArgs2,
    };
    use webview2_com::{NavigationCompletedEventHandler, ProcessFailedEventHandler};
    use windows::core::BOOL;
    use windows::core::Interface as _;
    use wry::WebViewExtWindows as _;

    let failed = |error| wry::Error::WebView2Error(webview2_com::Error::WindowsError(error));
    let core = webview.webview();
    let navigated = {
        let report = report.clone();
        NavigationCompletedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else {
                return Ok(());
            };
            let mut success = BOOL::default();
            // SAFETY: 引擎在界面线程上回调，参数在回调期间有效；出参指向本地变量。
            unsafe { args.IsSuccess(&mut success) }?;
            if success.as_bool() {
                return Ok(());
            }
            let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
            // SAFETY: 同上。
            unsafe { args.WebErrorStatus(&mut status) }?;
            // 被别的导航取代（换页、下载）不算没打开
            if status == COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED {
                return Ok(());
            }
            // 拿到了 HTTP 响应的（4xx、5xx：要登录、人机验证、地区限制的页面照样显示了）不算没打开；状态码为 0 即没
            // 拿到响应。旧的 WebView2 运行时没有这个接口，照没成报
            if let Ok(args) = args.cast::<ICoreWebView2NavigationCompletedEventArgs2>() {
                let mut code = 0;
                // SAFETY: 同上。
                unsafe { args.HttpStatusCode(&mut code) }?;
                if code > 0 {
                    return Ok(());
                }
            }
            report(Failure::LoadFailed(format!(
                "网页引擎报导航没成（WebErrorStatus {}）",
                status.0
            )));
            Ok(())
        }))
    };
    let mut token = 0;
    // SAFETY: 在界面线程上登记（WebView2 只能在建它的线程上用），网页此刻有效；处理器随网页一起释放。
    unsafe { core.add_NavigationCompleted(&navigated, &mut token) }.map_err(failed)?;
    let crashed = {
        let report = report.clone();
        ProcessFailedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else {
                return Ok(());
            };
            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
            // SAFETY: 引擎在界面线程上回调，参数在回调期间有效；出参指向本地变量。
            unsafe { args.ProcessFailedKind(&mut kind) }?;
            // 只有这几种让网页没了；GPU、工具进程与子框架的进程退出后引擎自己恢复
            if [
                COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
                COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
                COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE,
            ]
            .contains(&kind)
            {
                report(Failure::ProcessGone);
            }
            Ok(())
        }))
    };
    // SAFETY: 同 add_NavigationCompleted。
    unsafe { core.add_ProcessFailed(&crashed, &mut token) }.map_err(failed)?;
    Ok(())
}

/// 建好网页之后：WebKitGTK 接主资源加载失败与网页进程退出。
#[cfg(target_os = "linux")]
pub(crate) fn after_build(webview: &wry::WebView, report: &Report) -> wry::Result<()> {
    use webkit2gtk::{NetworkError, PolicyError, WebViewExt as _};
    use wry::WebViewExtUnix as _;

    let view = webview.webview();
    let failed = report.clone();
    view.connect_load_failed(move |_, _, _, error| {
        // 被别的导航取代、交给下载的不算没打开
        if !error.matches(NetworkError::Cancelled)
            && !error.matches(PolicyError::FrameLoadInterruptedByPolicyChange)
        {
            failed(Failure::LoadFailed(format!("网页加载失败：{error}")));
        }
        // 照常显示引擎自己的错误页
        false
    });
    let crashed = report.clone();
    view.connect_web_process_terminated(move |_, _| crashed(Failure::ProcessGone));
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn after_build(_: &wry::WebView, _: &Report) -> wry::Result<()> {
    Ok(())
}
