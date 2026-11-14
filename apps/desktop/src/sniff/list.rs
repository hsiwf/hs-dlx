//! 视频流一栏：网页的标题、主机与加载到哪一步，找到的流，网页要开的新窗口与要下载的文件，操作。

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use hs_m3u8_app::title_of;
use hs_m3u8_core::Url;

use super::found::{Check, Found};
use super::{MAX_FOUND, SUGGEST_AFTER, SniffEvent, SniffView};
use crate::links::http_url;
use crate::theme;
use crate::ui::{self, CAPTION_LINE, TEXT_LINE, Tone};

/// 不显示网页时列表的最大宽度。
const MAX_WIDTH: f32 = 760.;
/// 流前面的小同心环（见 [`ui::rings`]）的边长。
const RINGS: f32 = 44.;

/// 不显示网页时，列表独占主体。
pub(super) fn alone(list: Div) -> Div {
    list.flex_1().max_w(px(MAX_WIDTH)).pt(px(8.))
}

impl SniffView {
    pub(super) fn render_list(&self, cx: &mut Context<Self>) -> Div {
        let suggest = self.found.is_empty() && self.load.since.elapsed() >= SUGGEST_AFTER;
        let body: Vec<AnyElement> = match &self.webview {
            Err(reason) => vec![ui::error_text(reason.clone()).into_any_element()],
            // 打不开时原因写在下面，这里不再写在等
            Ok(_) if self.found.is_empty() && self.failure.is_some() => Vec::new(),
            Ok(_) if self.found.is_empty() => vec![
                div()
                    .text_size(px(13.))
                    .line_height(px(TEXT_LINE))
                    .text_color(theme::muted())
                    .child(match (self.load.shown.is_some(), suggest) {
                        (false, false) => "正在打开网页…",
                        (false, true) => "网页还没打开，可能连不上：打开网页看看",
                        (true, false) => "正在网页里找视频流…",
                        (true, true) => {
                            "还没找到。有的网页要登录、或点了播放才加载视频：打开网页弄好后，这里会列出来。"
                        }
                    })
                    .into_any_element(),
            ],
            Ok(_) => {
                let first = self.first_ready();
                self.found
                    .iter()
                    .enumerate()
                    .map(|(index, found)| self.render_row(index, found, first == Some(index), cx))
                    .collect()
            }
        };
        let mut notes = Vec::new();
        if self.overflow > 0 {
            notes.push(format!(
                "只列出前 {MAX_FOUND} 条，另有 {} 条没列出",
                self.overflow
            ));
        }
        if let Some(note) = &self.proxy_note {
            notes.push(note.clone());
        }
        let dropped = self.inbox.dropped();
        if dropped > 0 {
            notes.push(format!(
                "网页发来的消息太多或太长，丢了 {dropped} 条，可能有流没列出"
            ));
        }
        let popup = self.popup.clone().map(|url| {
            let target = url.clone();
            offer_line(
                format!("网页要开新窗口：{}", meta(&url)),
                "open-popup",
                "在这里打开",
                cx.listener(move |this, _, _, cx| {
                    this.popup = None;
                    this.open(target.as_str(), cx);
                }),
            )
        });
        let download = self.download.clone().map(|url| {
            let target = url.clone();
            offer_line(
                format!("网页要下载文件（没有下）：{}", meta(&url)),
                "probe-download",
                "探测它",
                cx.listener(move |this, _, _, cx| {
                    this.download = None;
                    this.open_download(target.clone(), cx);
                }),
            )
        });
        let failure = self
            .failure
            .as_ref()
            .map(|reason| ui::error_text(format!("{reason}：刷新后重新找")));
        let failed = self.failure.is_some();
        let actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .when(failed, |actions| {
                actions.child(ui::button(
                    "reload",
                    "刷新",
                    Tone::Glow,
                    cx.listener(|this, _, _, cx| {
                        this.navigate(wry::WebView::reload, "没能刷新", cx)
                    }),
                ))
            })
            .when(!self.page_shown && self.webview.is_ok(), |actions| {
                actions.child(ui::button(
                    "show-page",
                    "打开网页",
                    if suggest && !failed {
                        Tone::Glow
                    } else {
                        Tone::Plain
                    },
                    cx.listener(|this, _, _, cx| this.show_page(true, cx)),
                ))
            })
            // 找不到流时，打开的地址可能本身就是播放列表（缺请求头时源站回了网页）：交回新建页改请求头
            .when(self.found.is_empty(), |actions| {
                actions.child(ui::button(
                    "probe-opened",
                    "当作播放列表探测",
                    Tone::Plain,
                    cx.listener(|this, _, _, cx| this.open_opened(cx)),
                ))
            })
            .child(div().flex_1())
            .child(ui::button(
                "close-sniff",
                "关闭",
                Tone::Ghost,
                cx.listener(|_, _, _, cx| cx.emit(SniffEvent::Dismissed)),
            ));
        div()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.render_heading())
            .child(
                div()
                    .id("found")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .children(body),
            )
            .children(failure)
            .children(popup)
            .children(download)
            .children(notes.into_iter().map(ui::caption))
            .children(self.error.clone().map(ui::error_text))
            .child(actions)
    }

    /// 标题与主机，右边是网页加载到哪一步。
    fn render_heading(&self) -> Div {
        let host = self
            .load
            .shown
            .as_deref()
            .and_then(http_url)
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default();
        let state = match (&self.webview, self.load.shown.is_some()) {
            (Err(_), _) => None,
            _ if self.failure.is_some() => Some((theme::orange(), "打不开")),
            (Ok(_), false) => Some((theme::cyan(), "正在打开")),
            (Ok(_), true) if self.load.finished => Some((theme::cyan(), "已加载")),
            (Ok(_), true) => Some((theme::cyan(), "加载中")),
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_weight(FontWeight::BOLD)
                            .truncate()
                            .child(
                                self.title
                                    .clone()
                                    .unwrap_or_else(|| "网页里的视频流".to_owned()),
                            ),
                    )
                    .child(ui::num(ui::caption(host))),
            )
            .children(state.map(|(color, text)| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::dot(color, 7.))
                    .child(ui::caption(text.to_owned()))
            }))
    }

    /// 最该下载的一条：第一条探测好的主播放列表（有几档清晰度可挑），没有时第一条探测好的。
    fn first_ready(&self) -> Option<usize> {
        let ready = |master: bool| {
            self.found.iter().position(|found| {
                matches!(&found.check, Check::Ready { probe, .. } if !master || probe.master.is_some())
            })
        };
        ready(true).or_else(|| ready(false))
    }

    /// 一条流：同心环、名字、地址与探测说明，与能做的事：探测好的下载；没成的重试（重新读 Cookie），配好了请求头的
    /// 还能打开新建页改请求头再试。
    fn render_row(
        &self,
        index: usize,
        found: &Found,
        first: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (mut detail, failed) = match &found.check {
            Check::Waiting => ("等待探测".to_owned(), false),
            Check::Running => ("探测中…".to_owned(), false),
            Check::Ready { summary, .. } => (summary.clone(), false),
            Check::Failed { reason, .. } => (reason.clone(), true),
        };
        if found.skipped > 0 {
            detail.push_str(&format!(
                " · 没带 {} 个放不进请求头的 Cookie",
                found.skipped
            ));
        }
        let open = |label: &'static str, tone: Tone, cx: &mut Context<Self>| {
            let url = found.url.clone();
            ui::button(
                ("open-found", index),
                label,
                tone,
                cx.listener(move |this, _, _, cx| this.open_found(index, &url, cx)),
            )
        };
        let retry = |cx: &mut Context<Self>| {
            let url = found.url.clone();
            ui::button(
                ("retry-found", index),
                "重试",
                Tone::Plain,
                cx.listener(move |this, _, _, cx| this.retry(Some((index, &url)), cx)),
            )
        };
        let actions = match &found.check {
            Check::Waiting | Check::Running => Vec::new(),
            Check::Ready { .. } => vec![open(
                "下载",
                if first { Tone::Glow } else { Tone::Plain },
                cx,
            )],
            Check::Failed {
                headers: Some(_), ..
            } => vec![retry(cx), open("查看", Tone::Plain, cx)],
            Check::Failed { headers: None, .. } => vec![retry(cx)],
        };
        div()
            .flex()
            .items_center()
            .gap(px(14.))
            .p(px(12.))
            .rounded(px(14.))
            .border_1()
            .border_color(theme::edge())
            .bg(theme::panel())
            .child(ui::rings(RINGS, 4., 18.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(title_of(&found.url)),
                    )
                    .child(ui::num(
                        div()
                            .text_size(px(12.))
                            .line_height(px(CAPTION_LINE))
                            .text_color(theme::muted())
                            .truncate()
                            .child(SharedString::from(meta(&found.url))),
                    ))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(CAPTION_LINE))
                            .text_color(if failed {
                                theme::orange()
                            } else {
                                theme::soft()
                            })
                            .line_clamp(2)
                            .child(detail),
                    ),
            )
            .child(div().flex().flex_none().gap(px(8.)).children(actions))
            .into_any_element()
    }
}

/// 网页要做、没有放行的一件事（开新窗口、下载）与让用户接着做的按钮。
fn offer_line(
    text: String,
    id: &'static str,
    label: &'static str,
    act: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(div().flex_1().min_w_0().child(ui::caption(text)))
        .child(ui::button(id, label, Tone::Plain, act))
}

/// 流的说明：主机与路径；带查询参数的标出（同一个地址常带不同的令牌，只看主机与路径分不出来）。
fn meta(url: &Url) -> String {
    let mut text = format!("{}{}", url.host_str().unwrap_or_default(), url.path());
    if url.query().is_some() {
        text.push_str(" · 带参数");
    }
    text
}
