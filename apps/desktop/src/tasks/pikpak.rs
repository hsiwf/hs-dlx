//! PikPak 账号的登录状态：登录在系统浏览器里确认（设备授权），这边等确认；设置页显示它，云离线任务登录失效时也从这里
//! 重新登录。

use gpui_kit::{Context, Task};
use hs_m3u8_core::Url;

use super::Tasks;

/// PikPak 的登录状态。
#[derive(Default)]
pub struct PikPak {
    /// 登录了的用户编号；没登录时为 None，读不出存储时为原因
    pub user: Option<Result<String, String>>,
    /// 在登录：要一个登录码时为 None，拿到后为确认页与码
    pub login: Option<Option<Pending>>,
    /// 最近一次登录或退出没成的原因
    pub error: Option<String>,
    _login: Option<Task<()>>,
}

/// 等用户在浏览器里确认的登录。
#[derive(Clone)]
pub struct Pending {
    /// 确认页上显示的码，供用户核对
    pub user_code: String,
    /// 在系统浏览器里打开的确认页
    pub url: Url,
}

impl Tasks {
    pub fn pikpak(&self) -> &PikPak {
        &self.pikpak
    }

    /// 重读登录了的账号。
    pub(super) fn reload_pikpak(&mut self) {
        self.pikpak.user = match self.service.pikpak_user() {
            Ok(Some(user)) => Some(Ok(user)),
            Ok(None) => None,
            Err(error) => Some(Err(format!("读不出 PikPak 的账号：{error}"))),
        };
    }

    /// 开始登录 PikPak：要一个登录码，在系统浏览器里打开确认页，等用户确认；已在登录时什么也不做。
    pub fn login_pikpak(&mut self, cx: &mut Context<Self>) {
        if self.pikpak.login.is_some() {
            return;
        }
        let started = self.service.login_pikpak();
        self.pikpak.error = None;
        self.pikpak.login = Some(None);
        self.pikpak._login = Some(cx.spawn(async move |this, cx| {
            let login = match started.await {
                Ok(Ok(login)) => login,
                Ok(Err(error)) => return finish(&this, cx, Err(error.to_string())),
                Err(join) => return finish(&this, cx, Err(format!("要登录码时出错：{join}"))),
            };
            let pending = Pending {
                user_code: login.user_code.clone(),
                url: login.url.clone(),
            };
            let shown = this.update(cx, |this, cx| {
                cx.open_url(pending.url.as_str());
                this.pikpak.login = Some(Some(pending));
                cx.notify();
            });
            if shown.is_err() {
                return;
            }
            let result = match login.done.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(error.to_string()),
                Err(join) => Err(format!("等登录确认时出错：{join}")),
            };
            finish(&this, cx, result);
        }));
        cx.notify();
    }

    /// 不再等这次登录。
    pub fn cancel_pikpak_login(&mut self, cx: &mut Context<Self>) {
        self.pikpak.login = None;
        self.pikpak._login = None;
        cx.notify();
    }

    /// 在浏览器里再打开一次确认页。
    pub fn reopen_pikpak_login(&self, cx: &mut Context<Self>) {
        if let Some(Some(pending)) = &self.pikpak.login {
            cx.open_url(pending.url.as_str());
        }
    }

    /// 退出 PikPak：删掉存着的账号。
    pub fn logout_pikpak(&mut self, cx: &mut Context<Self>) {
        self.pikpak.error = self
            .service
            .logout_pikpak()
            .err()
            .map(|error| error.to_string());
        self.reload_pikpak();
        cx.notify();
    }
}

/// 登录有了结果：成了即重读账号，没成写下原因。
fn finish(
    this: &gpui_kit::WeakEntity<Tasks>,
    cx: &mut gpui_kit::AsyncApp,
    result: Result<(), String>,
) {
    // 等待期间应用退出时不再需要结果
    let _gone = this.update(cx, |this, cx| {
        // 跑着这段的任务留在 `_login` 里，下次登录时换掉
        this.pikpak.login = None;
        match result {
            Ok(()) => this.reload_pikpak(),
            Err(reason) => this.pikpak.error = Some(reason),
        }
        cx.notify();
    });
}
