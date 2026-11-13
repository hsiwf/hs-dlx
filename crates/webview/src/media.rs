//! 由网页引擎暂停或静音网页里的视频与音频。

#[cfg(target_os = "macos")]
pub(crate) fn suspend(webview: &wry::WebView, suspended: bool) -> Result<(), String> {
    use objc2::runtime::NSObjectProtocol as _;
    use objc2::sel;
    use wry::WebViewExtMacOS as _;

    let webview = webview.webview();
    // macOS 12 起才有；更早的系统上调了会因认不出方法而中止
    if !webview.respondsToSelector(sel!(setAllMediaPlaybackSuspended:completionHandler:)) {
        return Err("这个系统版本的网页引擎不能暂停播放".to_owned());
    }
    // SAFETY: 在界面线程上调（WKWebView 只能在主线程上用），网页视图此刻有效，上面已确认它认得这个方法；不要完成回调。
    unsafe { webview.setAllMediaPlaybackSuspended_completionHandler(suspended, None) };
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn suspend(webview: &wry::WebView, suspended: bool) -> Result<(), String> {
    use webkit2gtk::WebViewExt as _;
    use wry::WebViewExtUnix as _;

    webview.webview().set_is_muted(suspended);
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn suspend(webview: &wry::WebView, suspended: bool) -> Result<(), String> {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_8;
    use windows::core::Interface as _;
    use wry::WebViewExtWindows as _;

    let core = webview
        .webview()
        .cast::<ICoreWebView2_8>()
        .map_err(|error| format!("网页引擎太旧，不能静音：{error}"))?;
    // SAFETY: 在界面线程上调（WebView2 只能在建它的线程上用），网页视图此刻有效。
    unsafe { core.SetIsMuted(suspended) }.map_err(|error| format!("网页没能静音：{error}"))
}
