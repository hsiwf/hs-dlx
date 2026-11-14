//! 网页一栏：后退、前进、刷新、地址栏与收起，下面是网页本身。

use gpui_kit::component::input::Input;
use gpui_kit::{Context, Div, ParentElement as _, Styled as _, div, px};

use super::SniffView;
use crate::theme;
use crate::ui::{self, Tone};

/// 打开网页时右边列表一栏的宽度。
const ASIDE_WIDTH: f32 = 380.;
/// 窗口窄时右边一栏最多收到的宽度。
const ASIDE_MIN_WIDTH: f32 = 300.;
/// 网页一栏最窄多宽：常见网页的移动版布局从这里起。
const PAGE_MIN_WIDTH: f32 = 480.;

/// 显示网页时，列表在右边一栏。
pub(super) fn aside(list: Div) -> Div {
    list.flex_basis(px(ASIDE_WIDTH))
        .flex_shrink(1.)
        .min_w(px(ASIDE_MIN_WIDTH))
}

impl SniffView {
    /// 后退、前进或刷新：`act` 调网页的那个操作，`what` 为没做成时的说法。
    pub(super) fn navigate(
        &mut self,
        act: fn(&wry::WebView) -> wry::Result<()>,
        what: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(webview) = self.webview() {
            self.error = act(webview.read(cx).raw())
                .err()
                .map(|error| format!("{what}：{error}"));
        }
        cx.notify();
    }

    pub(super) fn render_page(&self, cx: &mut Context<Self>) -> Div {
        let nav = |id: &'static str,
                   label: &'static str,
                   act: fn(&wry::WebView) -> wry::Result<()>,
                   what: &'static str,
                   cx: &mut Context<Self>| {
            ui::button(
                id,
                label,
                Tone::Ghost,
                cx.listener(move |this, _, _, cx| this.navigate(act, what, cx)),
            )
        };
        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(12.))
            .py(px(10.))
            .border_b_1()
            .border_color(theme::line())
            .child(nav("back", "‹", wry::WebView::go_back, "没能后退", cx))
            .child(nav(
                "forward",
                "›",
                wry::WebView::go_forward,
                "没能前进",
                cx,
            ))
            .child(nav("reload", "↻", wry::WebView::reload, "没能刷新", cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(36.))
                    .ml(px(6.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(10.))
                    .bg(theme::raised())
                    .text_size(px(13.))
                    .child(Input::new(&self.address).appearance(false)),
            )
            .child(ui::button(
                "hide-page",
                "收起网页",
                Tone::Ghost,
                cx.listener(|this, _, _, cx| this.show_page(false, cx)),
            ));
        // 网页建不出来时没有「打开网页」，到不了这里
        let page = self
            .webview()
            .map(|webview| div().flex_1().min_h_0().child(webview.clone()));
        let trouble = self
            .webview()
            .and_then(|webview| webview.read(cx).trouble());
        div()
            .flex_1()
            .min_w(px(PAGE_MIN_WIDTH))
            .flex()
            .flex_col()
            // 原生网页盖住下边，圆角只留上边
            .rounded_t(px(18.))
            .border_1()
            .border_color(theme::line())
            .bg(theme::disc())
            .child(toolbar)
            .children(page)
            .children(trouble.map(|reason| div().p(px(10.)).child(ui::error_text(reason))))
    }
}
