//! 底部的粘贴栏：链接输入、探测状态、剪贴板提示与输入错误。

use gpui_kit::component::input::Input;
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use hs_m3u8_core::bare_url;

use super::{Page, Shell};
use crate::links::http_url;
use crate::tasks::Tasks;
use crate::theme;
use crate::ui::{self, CAPTION_LINE, TEXT_LINE, Tone};

/// 胶囊的高度。
const PILL_HEIGHT: f32 = 56.;
/// 下边距。
const BOTTOM: f32 = 24.;
/// 各行（剪贴板提示、胶囊、输入错误）之间的间距。
const LINE_GAP: f32 = 10.;
/// 粘贴栏的高：胶囊、下边距，上面一行剪贴板提示（小标题）、下面一行输入错误（正文），各带间距。
pub(super) const HEIGHT: f32 =
    PILL_HEIGHT + BOTTOM + (CAPTION_LINE + LINE_GAP) + (TEXT_LINE + LINE_GAP);

/// 粘贴栏胶囊的最大宽度。
const PILL_MAX_WIDTH: f32 = 880.;

impl Shell {
    pub(super) fn render_footer(&self, tasks: &Tasks, cx: &Context<Self>) -> impl IntoElement {
        let value = self.paste.read(cx).value();
        let typed = http_url(&value);
        let probing = self.new_task().and_then(|view| {
            let view = view.read(cx);
            (typed.as_ref() == Some(view.url())).then(|| view.phase())
        });
        let trailing = match probing {
            Some(phase) => div()
                .pr(px(14.))
                .text_size(px(12.))
                .text_color(theme::muted())
                .child(phase.label())
                .into_any_element(),
            // 粘贴栏空着时按了也没东西可探测，不抢眼
            None => ui::button(
                "probe",
                "探测",
                if value.trim().is_empty() {
                    Tone::Plain
                } else {
                    Tone::Glow
                },
                cx.listener(|this, _, window, cx| {
                    let text = this.paste.read(cx).value().to_string();
                    this.probe(&text, window, cx);
                }),
            )
            .into_any_element(),
        };
        // 待机页已经把剪贴板里的链接放在正中，别处只在粘贴栏空着时提一句
        let standby = matches!(self.page, Page::Task) && self.selected_row(tasks).is_none();
        let new_task = self.new_task().is_some();
        let hint = self
            .clipboard
            .clone()
            .filter(|_| !standby && value.is_empty() && !new_task)
            .map(|url| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .line_height(px(CAPTION_LINE))
                    .text_color(theme::muted())
                    .child(format!("剪贴板：{}", bare_url(&url)))
                    .child(
                        div()
                            .id("probe-clipboard-hint")
                            .text_color(theme::cyan())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::cyan_pale()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.probe_text(url.as_str(), window, cx)
                            }))
                            .child("探测"),
                    )
            });
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(LINE_GAP))
            .px(px(28.))
            .pb(px(BOTTOM))
            .children(hint)
            .child(
                div()
                    .w_full()
                    .max_w(px(PILL_MAX_WIDTH))
                    .h(px(PILL_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .pl(px(22.))
                    .pr(px(9.))
                    .rounded_full()
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::edge())
                    .child(ui::dot(theme::cyan(), 8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(15.))
                            .child(Input::new(&self.paste).appearance(false)),
                    )
                    .child(trailing),
            )
            .children(self.input_error.clone().map(ui::error_text))
    }
}
