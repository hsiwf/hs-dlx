//! 右上角的提示：要用户看到、关掉才消失的；正在退出时最上面一条说明在等什么。浮在内容上，不挤占布局。

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};

use super::Shell;
use crate::tasks::{Quit, Tasks};
use crate::theme;
use crate::ui::{self, PAD, TITLE_HEIGHT, Tone};

/// 提示的宽度、同时显示几条。
const WIDTH: f32 = 380.;
const SHOWN: usize = 4;

impl Shell {
    pub(super) fn render_notices(&self, tasks: &Tasks, cx: &Context<Self>) -> impl IntoElement {
        let quitting = (tasks.quit() == Quit::Waiting).then(|| {
            ui::card(theme::edge())
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(format!("正在退出：等 {} 个任务停下", tasks.busy()))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(ui::button(
                            "quit-now",
                            "立即退出",
                            Tone::Plain,
                            cx.listener(|this, _, _, cx| {
                                this.tasks.update(cx, |tasks, cx| tasks.leave(cx))
                            }),
                        ))
                        .child(ui::button(
                            "stay",
                            "不退出了",
                            Tone::Ghost,
                            cx.listener(|this, _, _, cx| {
                                this.tasks.update(cx, |tasks, cx| {
                                    tasks.stay();
                                    cx.notify();
                                })
                            }),
                        )),
                )
        });
        // 只显示最近几条；关掉一条露出更早的
        let notices: Vec<_> = tasks
            .notices()
            .iter()
            .rev()
            .take(SHOWN)
            .rev()
            .map(|notice| {
                let id = notice.id;
                ui::card(theme::warn_edge())
                    .flex()
                    .items_start()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(SharedString::from(notice.text.clone())),
                    )
                    .child(ui::button(
                        ("dismiss", id),
                        "知道了",
                        Tone::Ghost,
                        cx.listener(move |this, _, _, cx| {
                            this.tasks
                                .update(cx, |tasks, cx| tasks.dismiss_notice(id, cx))
                        }),
                    ))
            })
            .collect();
        div()
            .absolute()
            .top(px(TITLE_HEIGHT))
            .right(px(PAD))
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(quitting)
            .children(notices)
    }
}
