//! 新建页右边的选项栏：标题与摘要、清晰度之外的选择（音轨、输出、保存位置、请求头）、请求经哪里走与开始按钮；
//! 探测失败时为失败原因与能做的事（改请求头重新探测、重新探测、在嗅探页里打开地址）。

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, Div, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, div, px,
};
use hs_m3u8_app::{FailureKind, OutputKind};
use hs_m3u8_core::Probe;

use super::{NewTask, NewTaskEvent, OPTION_MIN_WIDTH, Probing, Route, duration_of};
use crate::ui::{self, Tone};
use crate::{format, links, theme};

/// 选项栏的宽度。
const OPTION_WIDTH: f32 = 320.;

impl NewTask {
    pub(super) fn render_side(&self, cx: &mut Context<Self>) -> Div {
        let host = self.url.host_str().unwrap_or_default().to_owned();
        let side = div()
            .flex_basis(px(OPTION_WIDTH))
            .flex_shrink(1.)
            .min_w(px(OPTION_MIN_WIDTH))
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(22.))
            .py(px(8.))
            .pr(px(8.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(20.))
                            .line_height(px(28.))
                            .font_weight(FontWeight::BOLD)
                            .line_clamp(2)
                            .child(self.title.clone()),
                    )
                    .child(ui::num(ui::caption(self.summary(&host)))),
            );
        match &self.probing {
            Probing::Running => side.child(div().flex_1()).child(ui::button(
                "cancel",
                "取消",
                Tone::Ghost,
                cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed)),
            )),
            Probing::Failed(failure) => {
                // 被拒或取到错误页的多半缺请求头：给出编辑框，带上再探测；再试也一样的不给重新探测。取到的不是
                // 播放列表的（多为网页）可以在嗅探页里打开它找视频流：地址不像播放列表时先做这个
                let headers = failure.kind.headers_may_help();
                let again = headers || failure.kind.retry_helps();
                let page = matches!(
                    failure.kind,
                    FailureKind::Malformed | FailureKind::Unsupported
                );
                let page_first = page && links::stream_url(self.url.as_str()).is_none();
                side.child(
                    ui::card(theme::error_edge())
                        .child(SharedString::from(failure.message.clone())),
                )
                .children(self.route_line())
                .when(headers, |side| {
                    side.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(ui::caption("请求头"))
                            .child(self.headers.clone()),
                    )
                })
                .child(div().flex_1())
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .flex_wrap()
                        .when(page_first, |row| {
                            row.child(self.sniff_button(Tone::Glow, cx))
                        })
                        .when(again, |row| {
                            row.child(ui::button(
                                "reprobe",
                                if headers {
                                    "带上请求头重新探测"
                                } else {
                                    "重新探测"
                                },
                                if page_first { Tone::Plain } else { Tone::Glow },
                                cx.listener(|this, _, window, cx| this.reprobe(window, cx)),
                            ))
                        })
                        .when(page && !page_first, |row| {
                            row.child(self.sniff_button(Tone::Plain, cx))
                        })
                        .child(ui::button(
                            "dismiss",
                            "返回",
                            Tone::Plain,
                            cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed)),
                        )),
                )
            }
            Probing::Ready(probe) => self.ready_side(side, probe, cx),
        }
    }

    /// 在嗅探页里打开这个地址的按钮。
    fn sniff_button(&self, tone: Tone, cx: &mut Context<Self>) -> Stateful<Div> {
        let url = self.url.clone();
        ui::button(
            "sniff",
            "在网页里找视频流",
            tone,
            cx.listener(move |_, _, _, cx| cx.emit(NewTaskEvent::Sniff(url.clone()))),
        )
    }

    /// 「网络」一行：这个地址的请求经哪个代理或为什么直连；还没读到系统代理时为 None。
    fn route_line(&self) -> Option<Div> {
        let line = match self.route.as_ref()? {
            Route::Proxy(proxy, source) => ui::num(
                div()
                    .text_size(px(13.))
                    .truncate()
                    .child(format!("经代理 {proxy}（{source}）")),
            ),
            Route::Direct(why) => div().text_size(px(13.)).child(format!("直连 · {why}")),
            Route::Broken(why) => ui::error_text(why.clone()),
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(ui::caption("网络"))
                .child(line),
        )
    }

    /// 标题下的一行：主机 · 时长 · 几档清晰度 · 几条音轨 · 直播。直播的时长是当前窗口的，不写。
    fn summary(&self, host: &str) -> String {
        let Probing::Ready(probe) = &self.probing else {
            return match self.probing {
                Probing::Running => format!("{host} · 正在读取播放列表"),
                _ => host.to_owned(),
            };
        };
        let mut parts = vec![host.to_owned()];
        if !probe.live {
            parts.push(format::clock(duration_of(probe)));
        }
        match &probe.master {
            Some(m) => {
                parts.push(format!("{} 档清晰度", m.variants.len()));
                let audio = m.audio.iter().filter(|a| a.separate).count();
                if audio > 0 {
                    parts.push(format!("{audio} 条音轨"));
                }
            }
            None => parts.push("单一清晰度".to_owned()),
        }
        if probe.live {
            parts.push("直播".to_owned());
        }
        parts.join(" · ")
    }

    /// 探测完成时的右栏：选项多了可以滚动，开始按钮总在底部。
    fn ready_side(&self, side: Div, probe: &Probe, cx: &mut Context<Self>) -> Div {
        let master = probe.master.as_ref();
        let duration = duration_of(probe);
        // （下标、名字、胶囊上写的、选中）
        let audio_options: Vec<(usize, String, String, bool)> = master
            .map(|m| {
                let group = m.variants[self.variant].audio_group.clone();
                m.audio
                    .iter()
                    .filter(|a| a.separate && Some(&a.group) == group.as_ref())
                    .map(|a| {
                        let name = a
                            .name
                            .clone()
                            .or_else(|| a.language.clone())
                            .unwrap_or_else(|| format!("音轨 {}", a.index + 1));
                        let on = self.audio == Some(a.index) || (self.audio.is_none() && a.default);
                        let label = if a.default {
                            format!("{name} · 默认")
                        } else {
                            name.clone()
                        };
                        (a.index, name, label, on)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let audio_label = audio_options
            .iter()
            .find(|(_, _, _, on)| *on)
            .map(|(_, name, _, _)| name.clone());

        let mut options = div()
            .id("options")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(22.));
        if !audio_options.is_empty() {
            let chips = audio_options.into_iter().map(|(index, _, label, on)| {
                ui::chip(
                    ("audio", index),
                    label,
                    on,
                    cx.listener(move |this, _, _, cx| {
                        this.audio = Some(index);
                        cx.notify();
                    }),
                )
            });
            options = options.child(group("音轨", chips));
        }
        let outputs = [OutputKind::Mp4, OutputKind::Hls, OutputKind::Both].map(|kind| {
            ui::chip(
                output_label(kind),
                output_label(kind),
                self.output == kind,
                cx.listener(move |this, _, _, cx| {
                    this.output = kind;
                    cx.notify();
                }),
            )
        });
        options = options.child(group("输出", outputs)).child(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(ui::caption("保存到"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(ui::num(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(format::path(&self.folder, self.home.as_deref())),
                        ))
                        .child(ui::button(
                            "folder",
                            "更改",
                            Tone::Plain,
                            cx.listener(|this, _, _, cx| this.change_folder(cx)),
                        )),
                ),
        );
        options = options.child(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(ui::caption("请求头"))
                .child(self.headers.clone()),
        );
        if let Some(line) = self.route_line() {
            options = options.child(line);
        }

        let picked = master.map(|m| &m.variants[self.variant]);
        let verb = if probe.live {
            "开始录制"
        } else {
            "开始下载"
        };
        let headline = if self.creating {
            "正在开始…".to_owned()
        } else {
            picked.map_or_else(
                || verb.to_owned(),
                |v| format!("{verb} · {}", format::variant(v)),
            )
        };
        let detail: Vec<String> = [
            picked
                .and_then(|v| v.bandwidth)
                .filter(|_| !probe.live)
                .map(|bw| format::size_bound(bw, duration)),
            audio_label,
            Some(output_label(self.output).to_owned()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let creating = self.creating;
        side.child(options)
            .children(self.error.clone().map(ui::error_text))
            .child(
                div()
                    .id("start")
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .w_full()
                    .px(px(18.))
                    .py(px(14.))
                    .rounded(px(18.))
                    .bg(theme::cyan())
                    .text_color(theme::on_cyan())
                    .shadow(ui::glow(theme::cyan(), 30., 0.35))
                    .when(creating, |s| s.opacity(0.6))
                    .when(!creating, |s| {
                        s.cursor_pointer()
                            .hover(|s| s.bg(theme::cyan_pale()))
                            .on_click(cx.listener(|this, _, window, cx| this.start(window, cx)))
                    })
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::BOLD)
                            .child(headline),
                    )
                    .child(ui::num(div().text_size(px(12.)).child(detail.join(" · ")))),
            )
            // 开始了就不能取消，任务建成后照常出现在列表里
            .when(!creating, |side| {
                side.child(div().flex().justify_center().child(ui::button(
                    "cancel",
                    "取消",
                    Tone::Ghost,
                    cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed)),
                )))
            })
    }
}

/// 输出选项上写的名字。
fn output_label(kind: OutputKind) -> &'static str {
    match kind {
        OutputKind::Mp4 => "MP4",
        OutputKind::Hls => "本地 HLS",
        OutputKind::Both => "两者",
    }
}

/// 带小标题的一组胶囊。
fn group(title: &'static str, chips: impl IntoIterator<Item = impl IntoElement>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(ui::caption(title))
        .child(div().flex().flex_wrap().gap(px(8.)).children(chips))
}
