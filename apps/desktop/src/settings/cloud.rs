//! 「网盘」一组：云离线用的网盘账号。

use gpui_kit::{Context, Div, ParentElement as _, Styled as _, div, px};

use super::{SettingsView, row};
use crate::tasks::Tasks;
use crate::ui::{self, Tone};

impl SettingsView {
    /// PikPak 一行：没登录时登录（在系统浏览器里确认），在登录时写确认页上的码，登录了可退出。
    pub(super) fn render_pikpak(&self, cx: &mut Context<Self>) -> Div {
        let tasks = self.tasks.clone();
        let act = move |key: &'static str,
                        label: &'static str,
                        tone: Tone,
                        f: fn(&mut Tasks, &mut Context<Tasks>)| {
            let tasks = tasks.clone();
            ui::button(key, label, tone, move |_, _, cx| tasks.update(cx, f))
        };
        let pikpak = self.tasks.read(cx).pikpak();
        let line = div().flex().items_center().gap(px(8.));
        let state = match (&pikpak.login, &pikpak.user) {
            (Some(None), _) => line.child(ui::field("在要登录码…").flex_1()).child(act(
                "pikpak-cancel",
                "取消",
                Tone::Ghost,
                |tasks, cx| tasks.cancel_pikpak_login(cx),
            )),
            (Some(Some(pending)), _) => div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    line.child(
                        ui::field(format!("在浏览器里登录并确认 · 码 {}", pending.user_code))
                            .flex_1(),
                    )
                    .child(act(
                        "pikpak-reopen",
                        "重新打开网页",
                        Tone::Plain,
                        |tasks, cx| tasks.reopen_pikpak_login(cx),
                    ))
                    .child(act(
                        "pikpak-cancel",
                        "取消",
                        Tone::Ghost,
                        |tasks, cx| tasks.cancel_pikpak_login(cx),
                    )),
                )
                .child(ui::caption("确认页上的码与这里的相同才确认")),
            (None, Some(Ok(user))) => line
                .child(ui::field(format!("已登录 · 用户 {user}")).flex_1())
                .child(act(
                    "pikpak-logout",
                    "退出登录",
                    Tone::Ghost,
                    |tasks, cx| tasks.logout_pikpak(cx),
                )),
            (None, user) => div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    line.child(act("pikpak-login", "登录", Tone::Plain, |tasks, cx| {
                        tasks.login_pikpak(cx)
                    })),
                )
                .children(match user {
                    Some(Err(reason)) => Some(ui::error_text(reason.clone())),
                    _ => None,
                }),
        };
        row(
            "PikPak",
            None,
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(state)
                .children(pikpak.error.clone().map(ui::error_text))
                .child(ui::caption(
                    "接口是非官方的，有违反服务条款、被封号的风险；大陆网络多半要经「下载」里设的代理才连得上",
                )),
        )
    }
}
