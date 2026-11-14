//! 新建页右边的检查面板：标题与主机；探测中为进行到哪一步与请求经哪里走；探测完成为探测结果的读数、清晰度之外的
//! 选择（音轨、输出、保存位置、请求头）与开始；探测失败为错误与能做的事（改请求头重新探测、重新探测、在网页里找
//! 视频流）。

use gpui_kit::{
    AnyElement, Context, Div, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use hs_m3u8_app::{Failure, OutputKind};
use hs_m3u8_core::Probe;

use super::{NewTask, NewTaskEvent, PageCheck, Probing, Route, duration_of};
use crate::format;
use crate::ui::{self, Step, Tone};

impl NewTask {
    pub(super) fn render_inspector(&self, cx: &mut Context<Self>) -> Div {
        let host = self.url.host_str().unwrap_or_default().to_owned();
        let title = self.title.clone();
        match &self.probing {
            Probing::Running => ui::inspector(
                title,
                host,
                [ui::steps([
                    ("读取播放列表".into(), Step::Now),
                    ("列出清晰度与音轨".into(), Step::Waiting),
                ])
                .into_any_element()]
                .into_iter()
                .chain(self.reads_with_route(Vec::new())),
                [self.cancel(Tone::Plain, cx)],
            ),
            Probing::Failed(_) if self.page_check == PageCheck::Opening => ui::inspector(
                title,
                host,
                [ui::steps([
                    ("取到的不是播放列表".into(), Step::Done),
                    ("在网页里打开，看是不是网页".into(), Step::Now),
                ])
                .into_any_element()],
                [self.cancel(Tone::Plain, cx)],
            ),
            Probing::Failed(failure) => self.failed_inspector(title, host, failure, cx),
            Probing::Ready(probe) => self.ready_inspector(title, host, probe, cx),
        }
    }

    /// 退出新建页的按钮，独占底栏一行。
    fn cancel(&self, tone: Tone, cx: &mut Context<Self>) -> AnyElement {
        ui::button(
            "cancel",
            "取消",
            tone,
            cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed)),
        )
        .w_full()
        .into_any_element()
    }

    /// 「网络」一格：这个地址的请求经哪个代理或为什么直连；代理用不了时为「用不了」与原因。还没读到系统代理时为 None。
    fn route(&self) -> Option<((SharedString, SharedString), Option<String>)> {
        let (value, broken) = match self.route.as_ref()? {
            Route::Proxy(proxy, source) => (format!("经 {proxy}（{source}）"), None),
            Route::Direct(why) => (format!("直连 · {why}"), None),
            Route::Broken(why) => ("用不了".to_owned(), Some(why.clone())),
        };
        Some((("网络".into(), value.into()), broken))
    }

    /// 读数格 `cells`，后面接一格占满一行的「网络」（读到系统代理之后），代理用不了时下面写原因。
    /// 两样都没有时为 None。
    fn reads_with_route(&self, cells: Vec<(SharedString, SharedString)>) -> Option<AnyElement> {
        let (route, broken) = self.route().unzip();
        if cells.is_empty() && route.is_none() {
            return None;
        }
        let shown = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(ui::reads(cells, route))
            .children(broken.flatten().map(ui::error_text))
            .into_any_element();
        Some(shown)
    }

    /// 探测失败：错误（与看过不是网页的说明）、请求经哪里走、缺请求头时的编辑框；底栏是重新探测与另找。被拒或取到错误
    /// 页的多半缺请求头，带上再探测；再试也一样的不给重新探测。不是从嗅探页来的还可以在网页里打开它找视频流。
    fn failed_inspector(
        &self,
        title: String,
        host: String,
        failure: &Failure,
        cx: &mut Context<Self>,
    ) -> Div {
        let headers = failure.kind.headers_likely_missing();
        let again = headers || failure.kind.retry_helps();
        let mut body = vec![ui::note(failure.message.clone(), true).into_any_element()];
        if let PageCheck::NotPage(note) = &self.page_check {
            body.push(ui::note(note.clone(), false).into_any_element());
        }
        body.extend(self.reads_with_route(Vec::new()));
        if headers {
            body.push(
                group("请求头", [self.headers.clone().into_any_element()]).into_any_element(),
            );
        }
        let mut foot = Vec::new();
        if again {
            foot.push(
                ui::primary(
                    "reprobe",
                    if headers {
                        "带上请求头重新探测"
                    } else {
                        "重新探测"
                    },
                    None,
                    Tone::Amber,
                    true,
                    cx.listener(|this, _, window, cx| this.reprobe(window, cx)),
                )
                .into_any_element(),
            );
        }
        let mut row = Vec::new();
        if !self.from_sniff {
            let url = self.url.clone();
            row.push(
                ui::button(
                    "sniff",
                    "在网页里找视频流",
                    Tone::Plain,
                    cx.listener(move |_, _, _, cx| cx.emit(NewTaskEvent::Sniff(url.clone()))),
                )
                .flex_1()
                .into_any_element(),
            );
        }
        // 没有别的按钮时「返回」独占一行，描边
        let alone = row.is_empty();
        let back = ui::button(
            "dismiss",
            "返回",
            if alone { Tone::Plain } else { Tone::Ghost },
            cx.listener(|_, _, _, cx| cx.emit(NewTaskEvent::Dismissed)),
        );
        row.push(if alone { back.flex_1() } else { back }.into_any_element());
        foot.push(div().flex().gap(px(10.)).children(row).into_any_element());
        ui::inspector(title, host, body, foot)
    }

    /// 探测完成：读数、选择（多了可以滚动）与开始。
    fn ready_inspector(
        &self,
        title: String,
        host: String,
        probe: &Probe,
        cx: &mut Context<Self>,
    ) -> Div {
        let master = probe.master.as_ref();
        let duration = duration_of(probe);
        // （下标、名字、选项上写的、选中）
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

        let mut cells: Vec<(SharedString, SharedString)> = vec![if probe.live {
            ("类型".into(), "直播".into())
        } else {
            ("时长".into(), format::clock(duration).into())
        }];
        cells.push((
            "清晰度".into(),
            master
                .map_or_else(|| "单一".to_owned(), |m| format!("{} 档", m.variants.len()))
                .into(),
        ));
        let separate = master.map_or(0, |m| m.audio.iter().filter(|a| a.separate).count());
        if separate > 0 {
            cells.push(("音轨".into(), format!("{separate} 条").into()));
        }
        let mut body: Vec<AnyElement> = self.reads_with_route(cells).into_iter().collect();

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
                .into_any_element()
            });
            body.push(group("音轨", [chips_row(chips)]).into_any_element());
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
            .into_any_element()
        });
        body.push(group("输出", [chips_row(outputs)]).into_any_element());
        body.push(
            group(
                "保存到",
                [div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::field(format::path(&self.folder, self.home.as_deref())).flex_1())
                    .child(ui::button(
                        "folder",
                        "更改",
                        Tone::Plain,
                        cx.listener(|this, _, _, cx| this.change_folder(cx)),
                    ))
                    .into_any_element()],
            )
            .into_any_element(),
        );
        body.push(group("请求头", [self.headers.clone().into_any_element()]).into_any_element());

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
        let mut foot: Vec<AnyElement> = self
            .error
            .clone()
            .map(|error| ui::error_text(error).into_any_element())
            .into_iter()
            .collect();
        foot.push(
            ui::primary(
                "start",
                headline,
                Some(detail.join(" · ").into()),
                Tone::Glow,
                !self.creating,
                cx.listener(|this, _, window, cx| this.start(window, cx)),
            )
            .into_any_element(),
        );
        // 开始了就不能取消，任务建成后照常出现在任务栏里
        if !self.creating {
            foot.push(self.cancel(Tone::Ghost, cx));
        }
        ui::inspector(title, host, body, foot)
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

/// 带小标题的一组。
fn group(title: &'static str, children: impl IntoIterator<Item = AnyElement>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(ui::caption(title))
        .children(children)
}

/// 一行可换行的选项。
fn chips_row(chips: impl IntoIterator<Item = AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .gap(px(8.))
        .children(chips)
        .into_any_element()
}
