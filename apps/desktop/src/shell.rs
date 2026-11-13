//! 主窗口：顶栏；左边一列任务小环；中间是选中任务的刻度环（新建时换成同心环，没有任务时是待机环）；右边是这个任务
//! 的信号；底部是粘贴栏。

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::{Local, TimeZone as _};
use gpui_kit::component::TitleBar;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, Entity, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, canvas, div, px,
};
use hs_m3u8_app::{Event, Events, Service, Status, TaskId};
use hs_m3u8_core::{Output, Progress, Stage, Url, bare_url};

use crate::new_task::{NewTask, NewTaskEvent, Phase};
use crate::ring::{self, Ticks};
use crate::signals::{self, Level, Signal};
use crate::ui::{self, Tone};
use crate::{TITLE_HEIGHT, format, theme};

/// 主角刻度环的绘制区域边长与半径，中间圆盘的内缩。
const HERO: f32 = 460.;
const HERO_RADIUS: f32 = 206.;
const DISC_INSET: f32 = 70.;

/// 下载速度与剩余时间的估计。
#[derive(Default)]
struct Rate {
    /// 本次运行第一次收到进度时已完成的分片数与时刻
    first: Option<(usize, Instant)>,
    /// 上一次进度里读到的字节数与时刻
    last: Option<(u64, Instant)>,
    /// 由相邻两次进度算出、平滑过的字节速度
    bytes_per_second: f64,
    /// 最近一次进度里已完成的分片数
    done: usize,
}

impl Rate {
    /// 记一次进度：本次运行读到的字节数 `received`、已完成的分片数 `done`。
    fn update(&mut self, received: u64, done: usize) {
        let now = Instant::now();
        if let Some((bytes, at)) = self.last {
            let elapsed = now.duration_since(at).as_secs_f64();
            if elapsed > 0.0 && received >= bytes {
                let rate = (received - bytes) as f64 / elapsed;
                // 进度事件每 100 毫秒一次，单次算出的速率跳动大；第一个值直接用
                self.bytes_per_second = if self.bytes_per_second == 0.0 {
                    rate
                } else {
                    self.bytes_per_second * 0.8 + rate * 0.2
                };
            }
        }
        self.last = Some((received, now));
        self.first.get_or_insert((done, now));
        self.done = done;
    }

    /// 还剩 `left` 片时估计的剩余时间：按本次运行以来的平均分片速度。分片常成批完成，相邻两次之间的速度不可用；
    /// 开始不到 2 秒或还没完成过分片时为 None。
    fn remaining(&self, left: usize) -> Option<Duration> {
        let (start, at) = self.first?;
        let elapsed = at.elapsed().as_secs_f64();
        let finished = self.done.checked_sub(start)?;
        (elapsed >= 2.0 && finished > 0 && left > 0)
            .then(|| Duration::from_secs_f64(left as f64 * elapsed / finished as f64))
    }
}

/// 列表里的一个任务：存储里的记录，加上运行时收到的进度、结果与信号。
struct Row {
    task: hs_m3u8_app::Task,
    progress: Option<Progress>,
    output: Option<Box<Output>>,
    rate: Rate,
    /// 按时间先后，最多 [`signals::KEEP`] 条
    signals: Vec<Signal>,
}

impl Row {
    fn new(task: hs_m3u8_app::Task) -> Row {
        // 存储里的时间是本应用写的 Unix 秒；换不成本地时刻（超出 chrono 的范围）时不写时刻
        let at = Local
            .timestamp_opt(task.created_at, 0)
            .earliest()
            .map_or_else(|| "—".to_owned(), |t| t.format("%m-%d %H:%M").to_string());
        let target = task.spec.output.options().target;
        let name = target
            .mp4()
            .or(target.hls())
            .expect("输出至少有 MP4 与本地 HLS 之一")
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut signals = Vec::new();
        signals::push(
            &mut signals,
            at,
            Level::Info,
            format!("新建 · 保存为 {name}"),
        );
        Row {
            signals,
            task,
            progress: None,
            output: None,
            rate: Rate::default(),
        }
    }

    fn signal(&mut self, level: Level, text: String) {
        let at = Local::now().format("%H:%M:%S").to_string();
        signals::push(&mut self.signals, at, level, text);
    }
}

pub struct Shell {
    service: Rc<Service>,
    rows: Vec<Row>,
    selected: Option<TaskId>,
    new_task: Option<Entity<NewTask>>,
    paste: Entity<InputState>,
    /// 新建任务默认保存到的目录；新建时换过就记住，到应用退出
    folder: PathBuf,
    home: Option<PathBuf>,
    notice: Option<String>,
    /// 剪贴板里一条还没下过、也不在探测中的 m3u8 链接
    clipboard: Option<Url>,
    /// 点过一次「放弃任务」、等着再点一次确认的任务
    confirm_discard: Option<TaskId>,
    _events: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    /// `link` 为启动参数里给的链接：有就填进粘贴栏并探测。
    pub fn new(
        service: Service,
        mut events: Events,
        folder: PathBuf,
        link: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let service = Rc::new(service);
        let (rows, notice) = match service.tasks() {
            Ok(tasks) => (tasks.into_iter().map(Row::new).collect(), None),
            Err(error) => (Vec::new(), Some(format!("读不出任务列表：{error}"))),
        };
        let paste = cx.new(|cx| InputState::new(window, cx).placeholder("粘贴链接，回车探测"));
        paste.update(cx, |input, cx| input.focus(window, cx));
        let typed = cx.subscribe_in(
            &paste,
            window,
            |this: &mut Self, input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let text = input.read(cx).value().to_string();
                    this.probe(&text, window, cx);
                }
                InputEvent::Change => cx.notify(),
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        let activated = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.refresh_clipboard(cx);
            }
        });
        let receiving = cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this.update(cx, |this, cx| this.apply(event, cx)).is_err() {
                    break;
                }
            }
        });
        let selected = rows.last().map(|r: &Row| r.task.id);
        let mut shell = Shell {
            service,
            rows,
            selected,
            new_task: None,
            paste,
            folder,
            home: dirs::home_dir(),
            notice,
            clipboard: None,
            confirm_discard: None,
            _events: receiving,
            _subscriptions: vec![typed, activated],
        };
        shell.refresh_clipboard(cx);
        if let Some(link) = link {
            shell
                .paste
                .update(cx, |input, cx| input.set_value(link.clone(), window, cx));
            shell.probe(&link, window, cx);
        }
        shell
    }

    fn apply(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Progress { id, progress } => {
                if let Some(row) = self.row_mut(id) {
                    row.rate.update(progress.received, progress.segments.done);
                    for (level, text) in signals::between(row.progress.as_ref(), &progress) {
                        row.signal(level, text);
                    }
                    row.progress = Some(*progress);
                }
            }
            Event::Ended { id, status, output } => {
                if let Some(row) = self.row_mut(id) {
                    let (level, text) = match &status {
                        Status::Done => (Level::Info, finished_text(output.as_deref())),
                        Status::Paused => (Level::Info, "已暂停，任务目录保留".to_owned()),
                        Status::Failed(message) => (Level::Error, message.clone()),
                        Status::Running => unreachable!("任务服务只在任务停下后发结束事件"),
                    };
                    row.signal(level, text);
                    row.task.status = status;
                    row.output = output;
                    row.rate = Rate::default();
                }
            }
            Event::StoreFailed { id, error } => {
                self.notice = Some(format!("任务 {} 的状态没能存下：{error}", id.0));
            }
        }
        cx.notify();
    }

    fn row_mut(&mut self, id: TaskId) -> Option<&mut Row> {
        self.rows.iter_mut().find(|row| row.task.id == id)
    }

    fn selected_row(&self) -> Option<&Row> {
        self.selected
            .and_then(|id| self.rows.iter().find(|r| r.task.id == id))
    }

    /// 重读剪贴板：是 http(s) 的 m3u8 链接、不是已有任务也不在探测中时记下，供一键探测。
    fn refresh_clipboard(&mut self, cx: &mut Context<Self>) {
        let found = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .and_then(|text| stream_url(&text));
        let probing = self
            .new_task
            .as_ref()
            .map(|view| view.read(cx).url().clone());
        self.clipboard = found.filter(|url| {
            !self.rows.iter().any(|row| &row.task.spec.url == url) && probing.as_ref() != Some(url)
        });
        cx.notify();
    }

    fn probe(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = match Url::parse(text.trim()) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => url,
            Ok(_) | Err(_) => {
                self.notice = Some("要 http 或 https 开头的链接".to_owned());
                cx.notify();
                return;
            }
        };
        self.notice = None;
        self.confirm_discard = None;
        if self.clipboard.as_ref() == Some(&url) {
            self.clipboard = None;
        }
        let (service, folder, home) =
            (self.service.clone(), self.folder.clone(), self.home.clone());
        let view = cx.new(|cx| NewTask::new(service, url, folder, home, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this: &mut Self, _, event: &NewTaskEvent, window, cx| {
                match event {
                    NewTaskEvent::Started(task) => this.started(task, window, cx),
                    NewTaskEvent::Dismissed => this.new_task = None,
                    NewTaskEvent::FolderChanged(folder) => this.folder = folder.clone(),
                }
                cx.notify();
            },
        );
        self._subscriptions.push(subscription);
        self.new_task = Some(view);
        cx.notify();
    }

    /// 把 `url` 填进粘贴栏并探测。
    fn probe_url(&mut self, url: &Url, window: &mut Window, cx: &mut Context<Self>) {
        let text = url.to_string();
        self.paste
            .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
        self.probe(&text, window, cx);
    }

    fn started(&mut self, task: &hs_m3u8_app::Task, window: &mut Window, cx: &mut Context<Self>) {
        self.new_task = None;
        self.selected = Some(task.id);
        self.rows.push(Row::new(task.clone()));
        self.paste
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn select(&mut self, id: TaskId, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.new_task = None;
        self.confirm_discard = None;
        cx.notify();
    }

    fn resume(&mut self, id: TaskId, cx: &mut Context<Self>) {
        match self.service.resume(id) {
            Ok(()) => {
                if let Some(row) = self.row_mut(id) {
                    row.task.status = Status::Running;
                    row.signal(Level::Info, "继续".to_owned());
                }
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        cx.notify();
    }

    /// 放弃任务：要删已下载内容的，第一次点只是要求确认。
    fn discard(&mut self, id: TaskId, confirmed: bool, cx: &mut Context<Self>) {
        if !confirmed {
            self.confirm_discard = Some(id);
            cx.notify();
            return;
        }
        self.confirm_discard = None;
        let handle = self.service.discard(id);
        cx.spawn(async move |this, cx| {
            let result = handle.await;
            // 等待期间窗口关掉时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(leftovers)) => {
                        this.rows.retain(|row| row.task.id != id);
                        if this.selected == Some(id) {
                            this.selected = this.rows.last().map(|r| r.task.id);
                        }
                        if let Some(first) = leftovers.first() {
                            this.notice = Some(format!("有东西没删掉：{first}"));
                        }
                    }
                    Ok(Err(error)) => this.notice = Some(format!("放弃失败：{error}")),
                    Err(join) => this.notice = Some(format!("放弃中途出错：{join}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// 剪贴板里的文字是 http(s) 的 m3u8 链接时为它。
fn stream_url(text: &str) -> Option<Url> {
    let url = Url::parse(text.trim()).ok()?;
    let is_stream = matches!(url.scheme(), "http" | "https")
        && url.path().to_ascii_lowercase().ends_with(".m3u8");
    is_stream.then_some(url)
}

/// 完成时的信号：写出了什么、多大。
fn finished_text(output: Option<&Output>) -> String {
    match output {
        Some(output) => {
            let kind = match (&output.mp4, &output.hls) {
                (Some(_), Some(_)) => "MP4 与本地 HLS",
                (Some(_), None) => "MP4",
                (None, _) => "本地 HLS",
            };
            format!("写出 {kind} · {}", format::bytes(output.bytes))
        }
        None => "完成".to_owned(),
    }
}

/// 一个任务此刻的样子。
struct Look {
    /// 圆盘里最上面的小字与它的颜色
    state: String,
    color: Hsla,
    ticks: Ticks,
    big: String,
    line: String,
    /// 列表小环：进度（0 到 1）、颜色与中间的简写
    fraction: f32,
    orb: Hsla,
    short: String,
}

fn look(row: &Row) -> Look {
    let progress = row.progress.clone().unwrap_or_default();
    let segments = progress.segments;
    let known = segments.total > 0;
    let fraction = if known {
        segments.done as f32 / segments.total as f32
    } else {
        0.0
    };
    let percent = format!("{:.0}%", fraction * 100.0);
    let mut ticks = Ticks::of(segments.total, segments.done, false);
    match &row.task.status {
        Status::Running if progress.stage == Stage::Writing => {
            let (kind, count) = match progress.writing {
                Some(w) => match (w.mp4, w.hls) {
                    (Some(c), _) => ("写出 MP4", Some(c)),
                    (None, Some(c)) => ("写出本地 HLS", Some(c)),
                    (None, None) => ("写出", None),
                },
                None => ("写出", None),
            };
            let written = count.map_or(0.0, |c| {
                (c.done as f32 / c.total.max(1) as f32).clamp(0.0, 1.0)
            });
            Look {
                state: kind.to_owned(),
                color: theme::cyan(),
                ticks: Ticks {
                    lit: ticks.count,
                    written: (written * ticks.count as f32) as usize,
                    ..ticks
                },
                big: format!("{:.0}%", written * 100.0),
                line: count.map_or_else(
                    || "不经网络".to_owned(),
                    |c| format!("已写 {} / {} 个文件 · 不经网络", c.done, c.total),
                ),
                fraction: written,
                orb: theme::cyan_pale(),
                short: format!("{:.0}%", written * 100.0),
            }
        }
        Status::Running if row.task.spec.live => Look {
            state: "● 录制中".to_owned(),
            color: theme::red(),
            ticks: Ticks {
                color: theme::red(),
                ..Ticks::of(segments.total, segments.done, true)
            },
            big: format::clock(progress.duration),
            line: format!("{} 片 · {}", segments.done, format::bytes(progress.bytes)),
            fraction: 1.0,
            orb: theme::red(),
            short: "REC".to_owned(),
        },
        Status::Running if !known => Look {
            state: "准备中".to_owned(),
            color: theme::cyan(),
            ticks: Ticks::of(0, 0, true),
            big: "—".to_owned(),
            line: "拉取播放列表".to_owned(),
            fraction: 0.0,
            orb: theme::cyan(),
            short: "…".to_owned(),
        },
        Status::Running => {
            ticks = Ticks::of(segments.total, segments.done, true);
            ticks.retrying = !progress.retrying.is_empty();
            let mut line = format!(
                "{} / {} 片 · {}/s",
                segments.done,
                segments.total,
                format::bytes(row.rate.bytes_per_second as u64)
            );
            if let Some(left) = row
                .rate
                .remaining(segments.total - segments.done.min(segments.total))
            {
                line.push_str(" · ");
                line.push_str(&format::remaining(left));
            }
            Look {
                state: "下载中".to_owned(),
                color: theme::cyan(),
                ticks,
                big: percent.clone(),
                line,
                fraction,
                orb: theme::cyan(),
                short: percent,
            }
        }
        Status::Paused => Look {
            state: "已暂停".to_owned(),
            color: theme::muted(),
            ticks: Ticks {
                color: theme::cyan_dim(),
                glow: false,
                ..ticks
            },
            big: if known {
                percent.clone()
            } else {
                "‖".to_owned()
            },
            line: if known {
                format!("{} / {} 片 · 继续时接着下", segments.done, segments.total)
            } else {
                "继续时从任务目录接着下".to_owned()
            },
            fraction,
            orb: theme::cyan_dim(),
            short: if known { percent } else { "‖".to_owned() },
        },
        Status::Failed(message) => Look {
            state: "需要处理".to_owned(),
            color: theme::orange(),
            ticks: Ticks {
                color: theme::finished(),
                glow: false,
                ..ticks
            },
            big: "!".to_owned(),
            line: message.clone(),
            fraction,
            orb: theme::orange(),
            short: "!".to_owned(),
        },
        Status::Done => Look {
            state: "已完成".to_owned(),
            color: theme::muted(),
            ticks: Ticks {
                lit: ticks.count,
                ..ticks
            },
            big: match &row.progress {
                Some(p) => format::clock(p.duration),
                None => "✓".to_owned(),
            },
            line: finished_text(row.output.as_deref()),
            fraction: 1.0,
            orb: theme::finished(),
            short: "✓".to_owned(),
        },
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_list = !self.rows.is_empty() || self.new_task.is_some();
        let mut body = div().flex_1().min_h_0().flex().gap(px(8.)).px(px(20.));
        if show_list {
            body = body.child(self.render_list(cx));
        }
        body = match (&self.new_task, self.selected_row()) {
            (Some(view), _) => body.child(div().flex_1().min_w_0().flex().child(view.clone())),
            (None, Some(row)) => {
                let (main, aside) = self.render_task(row, cx);
                body.child(main).child(aside)
            }
            (None, None) => body.child(self.render_standby(cx)),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::ground())
            .text_color(theme::text())
            .text_size(px(14.))
            .child(self.render_header())
            .child(body)
            .child(self.render_footer(cx))
    }
}

impl Shell {
    fn render_header(&self) -> impl IntoElement {
        let running: Vec<&Row> = self
            .rows
            .iter()
            .filter(|r| r.task.status == Status::Running)
            .collect();
        let status = if running.is_empty() {
            String::new()
        } else {
            let speed: f64 = running.iter().map(|r| r.rate.bytes_per_second).sum();
            format!(
                "{} 个进行中 · ↓ {}/s",
                running.len(),
                format::bytes(speed as u64)
            )
        };
        TitleBar::new()
            .h(px(TITLE_HEIGHT))
            .bg(theme::ground())
            .border_color(theme::ground())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .ml(px(14.))
                    .child(
                        canvas(
                            |_, _, _| {},
                            |bounds, _, window, _| {
                                ring::paint_arc(
                                    bounds,
                                    7.,
                                    2.5,
                                    0.72,
                                    theme::cyan(),
                                    theme::line(),
                                    window,
                                );
                            },
                        )
                        .size(px(18.)),
                    )
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::BOLD)
                            .child("hs-m3u8"),
                    ),
            )
            .child(ui::num(
                div()
                    .pr(px(28.))
                    .text_size(px(13.))
                    .text_color(theme::muted())
                    .child(status),
            ))
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut items: Vec<AnyElement> = Vec::new();
        if let Some(view) = &self.new_task {
            items.push(new_task_item(view.read(cx).phase()).into_any_element());
        }
        let dim = self.new_task.is_some();
        for row in self.rows.iter().rev() {
            items.push(self.render_orb(row, dim, cx).into_any_element());
        }
        div()
            .id("tasks")
            .w(px(300.))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.))
            .py(px(8.))
            .overflow_y_scroll()
            .children(items)
    }

    fn render_orb(&self, row: &Row, dim: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let id = row.task.id;
        let look = look(row);
        let on = self.selected == Some(id) && self.new_task.is_none();
        let (fraction, color) = (look.fraction, look.orb);
        div()
            .id(("task", id.0 as u64))
            .flex()
            .items_center()
            .gap(px(14.))
            .p(px(12.))
            .rounded(px(14.))
            .border_1()
            .border_color(if on {
                theme::edge()
            } else {
                gpui_kit::transparent_black()
            })
            .when(on, |s| s.bg(theme::selected()))
            .when(!on, |s| s.hover(|s| s.bg(theme::raised())))
            .when(dim, |s| s.opacity(0.55))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.select(id, cx)))
            .child(
                div()
                    .relative()
                    .size(px(52.))
                    .flex_none()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                ring::paint_arc(
                                    bounds,
                                    23.5,
                                    5.,
                                    fraction,
                                    color,
                                    theme::line(),
                                    window,
                                );
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(ui::num(
                        div()
                            .absolute()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.))
                            .text_color(theme::soft())
                            .child(look.short),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .min_w_0()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(row.task.spec.title.clone()),
                    )
                    .child(ui::num(
                        div()
                            .text_size(px(12.))
                            .text_color(theme::muted())
                            .truncate()
                            .child(format!("{} · {}", look.state, look.line)),
                    )),
            )
    }

    fn render_task(&self, row: &Row, cx: &mut Context<Self>) -> (Div, Div) {
        let look = look(row);
        let ticks = look.ticks;
        let hero = hero(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| ring::paint_ticks(bounds, HERO_RADIUS, &ticks, window),
            )
            .absolute()
            .size_full()
            .into_any_element(),
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(look.color)
                        .child(look.state),
                )
                .child(ui::num(
                    div()
                        .text_size(px(64.))
                        .line_height(px(70.))
                        .font_weight(FontWeight::BOLD)
                        .child(look.big),
                ))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_center()
                        .line_clamp(2)
                        .child(row.task.spec.title.clone()),
                )
                .child(ui::num(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::muted())
                        .text_center()
                        .line_clamp(3)
                        .child(SharedString::from(look.line)),
                )),
        );

        let main = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(24.))
            .pb(px(16.))
            .child(hero)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(10.))
                    .children(self.actions(row, cx)),
            );

        // 最新的在最上面，多了可以滚动
        let aside = div()
            .w(px(300.))
            .flex_none()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .py(px(8.))
            .pr(px(8.))
            .child(ui::caption("信号"))
            .child(
                div()
                    .id("signals")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .pb(px(16.))
                    .children(row.signals.iter().rev().map(signal_card)),
            );
        (main, aside)
    }

    fn actions(&self, row: &Row, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let id = row.task.id;
        let confirming = self.confirm_discard == Some(id);
        let discard = |label: &'static str, confirm_label: &'static str, cx: &mut Context<Self>| {
            ui::button(
                "discard",
                if confirming { confirm_label } else { label },
                Tone::Plain,
                cx.listener(move |this, _, _, cx| this.discard(id, confirming, cx)),
            )
            .when(confirming, |b| {
                b.border_color(theme::orange()).text_color(theme::orange())
            })
            .into_any_element()
        };
        let mut actions = Vec::new();
        match &row.task.status {
            Status::Running if row.task.spec.live => actions.push(
                ui::button(
                    "stop",
                    "停止并写出",
                    Tone::Rec,
                    cx.listener(move |this, _, _, cx| {
                        this.service.stop(id);
                        cx.notify();
                    }),
                )
                .into_any_element(),
            ),
            Status::Running => {
                actions.push(
                    ui::button(
                        "pause",
                        "暂停",
                        Tone::Plain,
                        cx.listener(move |this, _, _, _| this.service.pause(id)),
                    )
                    .into_any_element(),
                );
                actions.push(discard("放弃任务", "再点一次：删除已下的分片", cx));
            }
            Status::Paused | Status::Failed(_) => {
                actions.push(
                    ui::button(
                        "resume",
                        "继续",
                        Tone::Glow,
                        cx.listener(move |this, _, _, cx| this.resume(id, cx)),
                    )
                    .into_any_element(),
                );
                actions.push(discard("放弃任务", "再点一次：删除已下的分片", cx));
            }
            Status::Done => {
                let target = row.task.spec.output.options().target;
                let mp4 = target
                    .mp4()
                    .filter(|p| p.is_file())
                    .map(|p| p.to_path_buf());
                let shown = mp4
                    .clone()
                    .or_else(|| target.hls().filter(|p| p.exists()).map(|p| p.to_path_buf()));
                if let Some(mp4) = mp4 {
                    actions.push(
                        ui::button("play", "▶ 播放", Tone::Glow, move |_, _, cx| {
                            cx.open_with_system(&mp4)
                        })
                        .into_any_element(),
                    );
                }
                if let Some(shown) = shown {
                    actions.push(
                        ui::button(
                            "reveal",
                            "在访达中显示",
                            Tone::Plain,
                            move |_, _, cx| cx.reveal_path(&shown),
                        )
                        .into_any_element(),
                    );
                }
                actions.push(
                    ui::button(
                        "forget",
                        "从列表移除",
                        Tone::Ghost,
                        cx.listener(move |this, _, _, cx| this.discard(id, true, cx)),
                    )
                    .into_any_element(),
                );
            }
        }
        actions
    }

    fn render_standby(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hero = hero(
            canvas(
                |_, _, _| {},
                |bounds, _, window, _| ring::paint_standby(bounds, HERO_RADIUS, window),
            )
            .absolute()
            .size_full()
            .into_any_element(),
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::cyan())
                        .child("就绪"),
                )
                .child(
                    div()
                        .text_size(px(30.))
                        .font_weight(FontWeight::BOLD)
                        .child("粘贴链接"),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme::muted())
                        .child("m3u8 · 回车探测"),
                ),
        );
        let suggestion = self.clipboard.clone().map(|url| {
            let shown = bare_url(&url).to_string();
            div()
                .flex()
                .items_center()
                .gap(px(14.))
                .w(px(560.))
                .pl(px(20.))
                .pr(px(10.))
                .py(px(10.))
                .rounded(px(18.))
                .bg(theme::panel())
                .border_1()
                .border_color(theme::edge())
                .child(ui::dot(theme::cyan(), 8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(ui::caption("剪贴板里的链接"))
                        .child(div().text_size(px(13.)).truncate().child(shown)),
                )
                .child(ui::button(
                    "probe-clipboard",
                    "探测",
                    Tone::Glow,
                    cx.listener(move |this, _, window, cx| this.probe_url(&url, window, cx)),
                ))
        });
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(28.))
            .pb(px(16.))
            .child(hero)
            .children(suggestion)
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let value = self.paste.read(cx).value();
        let probing = self.new_task.as_ref().and_then(|view| {
            let view = view.read(cx);
            (value.trim() == view.url().as_str()).then(|| view.phase())
        });
        let trailing = match probing {
            Some(phase) => div()
                .pr(px(14.))
                .text_size(px(12.))
                .text_color(theme::muted())
                .child(match phase {
                    Phase::Probing => "正在探测",
                    Phase::Ready => "探测完成",
                    Phase::Failed => "探测失败",
                })
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
        let standby = self.new_task.is_none() && self.selected_row().is_none();
        let hint = self
            .clipboard
            .clone()
            .filter(|_| !standby && value.is_empty() && self.new_task.is_none())
            .map(|url| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .text_color(theme::muted())
                    .child(format!("剪贴板：{}", bare_url(&url)))
                    .child(
                        div()
                            .id("probe-clipboard-hint")
                            .text_color(theme::cyan())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::cyan_pale()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.probe_url(&url, window, cx)
                            }))
                            .child("探测"),
                    )
            });
        let notice = self.notice.clone().map(|text| {
            div()
                .text_size(px(13.))
                .text_color(theme::orange())
                .child(SharedString::from(text))
        });
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.))
            .px(px(28.))
            .pt(px(8.))
            .pb(px(24.))
            .children(hint)
            .child(
                div()
                    .w_full()
                    .max_w(px(880.))
                    .h(px(56.))
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
            .children(notice)
    }
}

/// 主角环：`ring` 是撑满整块区域的画布，中间压一个深色圆盘，`center` 放在圆盘里。
fn hero(ring: AnyElement, center: Div) -> Div {
    let disc = HERO - 2. * DISC_INSET;
    div()
        .relative()
        .size(px(HERO))
        .flex_none()
        .child(ring)
        .child(
            div()
                .absolute()
                .top(px(DISC_INSET))
                .left(px(DISC_INSET))
                .size(px(disc))
                .rounded_full()
                .bg(theme::disc()),
        )
        .child(
            div()
                .absolute()
                .top(px(DISC_INSET))
                .left(px(DISC_INSET))
                .size(px(disc))
                .px(px(36.))
                .flex()
                .items_center()
                .justify_center()
                .child(center),
        )
}

fn new_task_item(phase: Phase) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(14.))
        .p(px(12.))
        .rounded(px(14.))
        .border_1()
        .border_color(theme::edge())
        .bg(theme::selected())
        .child(
            canvas(
                |_, _, _| {},
                |bounds, _, window, _| {
                    ring::paint_dashed(bounds, 25., 2., 16, theme::cyan(), window);
                },
            )
            .size(px(52.))
            .flex_none(),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child("新任务"))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::muted())
                        .child(match phase {
                            Phase::Probing => "正在探测",
                            Phase::Ready => "探测完成 · 挑一档开始",
                            Phase::Failed => "探测失败",
                        }),
                ),
        )
}

fn signal_card(signal: &Signal) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(14.))
        .py(px(12.))
        .rounded(px(12.))
        .bg(theme::panel())
        .border_1()
        .border_color(match signal.level {
            Level::Info => theme::line(),
            Level::Warn => theme::warn_edge(),
            Level::Error => theme::error_edge(),
        })
        .child(ui::num(
            div()
                .flex()
                .justify_between()
                .text_size(px(11.))
                .text_color(theme::muted())
                .child(signal.at.clone())
                .when(signal.repeat > 1, |s| {
                    s.child(format!("×{}", signal.repeat))
                }),
        ))
        .child(
            div()
                .text_size(px(13.))
                .line_height(px(19.))
                .child(SharedString::from(signal.text.clone())),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_m3u8_links_are_picked_from_the_clipboard() {
        assert!(stream_url(" https://a.example/v/index.m3u8?t=1 \n").is_some());
        assert!(stream_url("https://a.example/v/INDEX.M3U8").is_some());
        assert!(stream_url("https://a.example/v/clip.mp4").is_none());
        assert!(stream_url("file:///tmp/x.m3u8").is_none());
        assert!(stream_url("看这个 https://a.example/x.m3u8").is_none());
    }
}
