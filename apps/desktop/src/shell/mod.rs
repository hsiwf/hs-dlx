//! 主窗口：顶栏；左边一列任务小环；中间是选中任务的刻度环（新建时换成同心环，没有任务时是待机环）；右边是这个任务
//! 的信号；底部是粘贴栏；右上角是要用户关掉的提示。这里管状态与事件；新建、对任务的操作、退出与各块的画法在子模块里。

mod creating;
mod live_view;
mod notices;
mod operations;
mod paste_bar;
mod quit;
mod row;
mod task_list;
mod task_view;
mod title_bar;

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Task, Window, div, px,
};
use hs_m3u8_app::{Event, Events, Finished, Service, Status, TaskId};
use hs_m3u8_core::{Stage, Url};

use self::row::Row;
use crate::headers::HeadersEditor;
use crate::links;
use crate::new_task;
use crate::new_task::NewTask;
use crate::signals::Level;
use crate::theme;
use crate::ui::{GAP, PAD, TITLE_HEIGHT, max};

/// 窗口的最小宽：放得下列表收到最窄时、最宽的一个视图（待机页没有列表，比它们窄）。
pub const MIN_WIDTH: f32 =
    2. * PAD + task_list::MIN_WIDTH + GAP + max(new_task::MIN_WIDTH, task_view::MIN_WIDTH);
/// 窗口的最小高：放得下顶栏、最高的一个视图与粘贴栏。
pub const MIN_HEIGHT: f32 = TITLE_HEIGHT
    + max(
        max(task_view::MIN_HEIGHT, task_view::STANDBY_MIN_HEIGHT),
        new_task::MIN_HEIGHT,
    )
    + paste_bar::HEIGHT;

/// 多久读一次保存目录所在盘的可用空间。
const FREE_SPACE_INTERVAL: Duration = Duration::from_secs(10);

/// `folder`（还不存在时取最近的已存在的上级）所在盘对本用户可用的字节数；读不出时为原因。
fn free_space(folder: &std::path::Path) -> Result<u64, String> {
    let existing = folder
        .ancestors()
        .find(|path| path.exists())
        .ok_or_else(|| format!("{} 及其上级都不存在", folder.display()))?;
    fs4::available_space(existing).map_err(|error| error.to_string())
}

/// 一条要用户看到、关掉才消失的提示（操作没成、没删干净的东西）。
struct Notice {
    id: u64,
    text: String,
}

pub struct Shell {
    service: Rc<Service>,
    rows: Vec<Row>,
    selected: Option<TaskId>,
    /// 新建视图与对它的订阅：视图关掉时订阅一起释放
    new_task: Option<(Entity<NewTask>, Subscription)>,
    paste: Entity<InputState>,
    /// 新建任务默认保存到的目录；新建时换过就记住，到应用退出
    folder: PathBuf,
    home: Option<PathBuf>,
    /// 粘贴栏里的内容不能探测的原因；再探测或改了内容时清掉
    input_error: Option<String>,
    notices: Vec<Notice>,
    next_notice: u64,
    /// 剪贴板里一条还没下过、也不在探测中的 m3u8 链接
    clipboard: Option<Url>,
    /// 点过一次「放弃任务」、等着再点一次确认的任务
    confirm_discard: Option<TaskId>,
    /// 正在改请求头的失败任务与编辑框
    editing_headers: Option<(TaskId, Entity<HeadersEditor>)>,
    /// 用户选了退出：等运行中的任务都停下就退出；期间不开始新的运行
    quitting: bool,
    /// 在服务里进行中的新建：退出要等它们（建成的随即暂停）
    creating: usize,
    /// 新建期间收到的、列表里还没有的任务的结束事件：新建的结果与任务的事件走两条路，结束可能先到；加进那一行时再处理
    early: Vec<Event>,
    _events: Task<()>,
    /// 保存目录所在盘的可用空间（字节）：还没读到时为 None，读不出时为原因
    free_space: Option<Result<u64, String>>,
    _ticker: Task<()>,
    _free_space: Task<()>,
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
        let now = Instant::now();
        let mut notices = Vec::new();
        let rows: Vec<Row> = match service.tasks() {
            Ok(tasks) => tasks.into_iter().map(|task| Row::new(task, now)).collect(),
            Err(error) => {
                notices.push(Notice {
                    id: 0,
                    text: format!("读不出任务列表：{error}"),
                });
                Vec::new()
            }
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
                InputEvent::Change => {
                    this.input_error = None;
                    cx.notify();
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        // 从程序坞或菜单退出不经关窗：停下所有任务、补写状态（最多等 2 秒），退出已不可撤回
        let quitting = cx.on_app_quit(|this: &mut Self, _| {
            // 没有窗口能显示了；补写不进的下次打开时按暂停处理（见 Service::close）
            let _unsaved = this.service.close();
            async {}
        });
        let activated = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.refresh_clipboard(cx);
            }
        });
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            // 视图已不在时没有要保护的任务
            this.update(cx, |shell, cx| shell.should_close(window, cx))
                .unwrap_or(true)
        });
        let receiving = cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this.update(cx, |this, cx| this.apply(event, cx)).is_err() {
                    break;
                }
            }
        });
        // 速度按时间窗算，没有新进度时也要随时间降下来：有任务在运行时每秒重画一次
        // 盘余随下载变化，读一次很快，但网络盘上可能卡住：在后台读，每 10 秒一次
        let free_space = cx.spawn(async move |this, cx| {
            loop {
                let Ok(folder) = this.read_with(cx, |this, _| this.folder.clone()) else {
                    break;
                };
                let read = cx
                    .background_executor()
                    .spawn(async move { free_space(&folder) })
                    .await;
                let alive = this.update(cx, |this, cx| {
                    this.free_space = Some(read);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(FREE_SPACE_INTERVAL).await;
            }
        });
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |this, cx| {
                    if this.rows.iter().any(|r| r.task.status == Status::Running) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let selected = rows.last().map(|r| r.task.id);
        let mut shell = Shell {
            service,
            rows,
            selected,
            new_task: None,
            paste,
            folder,
            home: dirs::home_dir(),
            input_error: None,
            next_notice: notices.len() as u64,
            notices,
            clipboard: None,
            confirm_discard: None,
            editing_headers: None,
            quitting: false,
            creating: 0,
            early: Vec::new(),
            _events: receiving,
            free_space: None,
            _ticker: ticker,
            _free_space: free_space,
            _subscriptions: vec![typed, activated, quitting],
        };
        let ids: Vec<TaskId> = shell.rows.iter().map(|r| r.task.id).collect();
        for id in ids {
            shell.after_stop(id, cx);
        }
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
        if let Event::Ended { id, .. } = &event
            && self.creating > 0
            && self.row(*id).is_none()
        {
            self.early.push(event);
            return;
        }
        match event {
            Event::Progress { id, progress } => {
                if let Some(row) = self.row_mut(id) {
                    row.rate
                        .update(Instant::now(), progress.received, progress.segments.done);
                    for (level, text) in crate::signals::between(row.progress.as_ref(), &progress) {
                        row.signal(level, text);
                    }
                    if progress.stage == Stage::Recording
                        && let Some(session) =
                            row.timeline.observe(row.progress.as_ref(), &progress)
                    {
                        row.signal(Level::Warn, format!("来源接不上，另起会话 {}", session + 1));
                    }
                    row.progress = Some(*progress);
                }
            }
            Event::Ended {
                id,
                status,
                result,
                total,
                leftovers,
                unsaved,
            } => {
                let title = self.title(id);
                let mut problems: Vec<String> = leftovers
                    .iter()
                    .map(|leftover| format!("「{title}」完成时没删掉：{leftover}"))
                    .collect();
                if let Some(error) = unsaved {
                    problems.push(format!(
                        "「{title}」已停下，但状态没能写进存储（{error}），之后每次写存储时会再写"
                    ));
                }
                self.ended(id, status, result, total, problems, cx);
            }
            Event::TotalUnsaved { id, error } => {
                let title = self.title(id);
                self.notify(format!(
                    "没能记下「{title}」的总片数：{error}；任务照常下载，停下时再记"
                ));
            }
            Event::Orphaned { paths, reason } => {
                let paths: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                self.notify(format!(
                    "新建任务时存储写不进，任务已取消，但留下了不在任何任务名下的 {}：{reason}",
                    paths.join("、")
                ));
                if self.quitting {
                    self.cancel_quit("有要处理的东西，没有退出".to_owned());
                }
            }
        }
        cx.notify();
    }

    /// 任务停下：改状态、总片数与结果，写信号，作废之前读到的任务目录内容后重读（或查输出）；`problems` 为要用户
    /// 看的提示（没删掉的东西、状态没写进存储）。退出中遇到失败或要提示的就不退出，留给用户看。
    fn ended(
        &mut self,
        id: TaskId,
        status: Status,
        result: Option<Finished>,
        total: Option<usize>,
        problems: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let title = self.title(id);
        let failed = matches!(status, Status::Failed(_));
        if let Some(row) = self.row_mut(id) {
            // 放弃时任务先停下，这时的「暂停」不是用户要的，不写
            if !row.discarding {
                let (level, text) = ended_signal(&status, &row.task.spec.output, result.as_ref());
                row.signal(level, text);
            }
            row.task.status = status;
            if result.is_some() {
                row.task.result = result;
            }
            if total.is_some() {
                row.task.total = total;
            }
            row.forget_disk();
        }
        if self.quitting && (failed || !problems.is_empty()) {
            self.cancel_quit(format!("「{title}」停下时有要处理的，没有退出"));
        }
        for text in problems {
            self.notify(text);
        }
        self.after_stop(id, cx);
        self.quit_if_idle(cx);
    }

    fn row(&self, id: TaskId) -> Option<&Row> {
        self.rows.iter().find(|row| row.task.id == id)
    }

    fn row_mut(&mut self, id: TaskId) -> Option<&mut Row> {
        self.rows.iter_mut().find(|row| row.task.id == id)
    }

    fn selected_row(&self) -> Option<&Row> {
        self.selected.and_then(|id| self.row(id))
    }

    /// 从列表里去掉一行；选中的是它时改选最新的一行。
    fn remove_row(&mut self, id: TaskId) {
        self.rows.retain(|row| row.task.id != id);
        if self.selected == Some(id) {
            self.selected = self.rows.last().map(|r| r.task.id);
        }
    }

    /// 列表里的名字；行已不在（放弃做完）时写编号。
    fn title(&self, id: TaskId) -> String {
        self.row(id)
            .map_or_else(|| id.to_string(), |r| r.task.spec.title.clone())
    }

    fn notify(&mut self, text: String) {
        self.notices.push(Notice {
            id: self.next_notice,
            text,
        });
        self.next_notice += 1;
    }

    fn dismiss_notice(&mut self, id: u64, cx: &mut Context<Self>) {
        self.notices.retain(|n| n.id != id);
        cx.notify();
    }

    /// 重读剪贴板：是 http(s) 的 m3u8 链接、不是已有任务也不在探测中时记下，供一键探测。
    fn refresh_clipboard(&mut self, cx: &mut Context<Self>) {
        let found = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .and_then(|text| links::stream_url(&text));
        let probing = self
            .new_task
            .as_ref()
            .map(|(view, _)| view.read(cx).url().clone());
        self.clipboard = found.filter(|url| {
            !self.rows.iter().any(|row| &row.task.spec.url == url) && probing.as_ref() != Some(url)
        });
        cx.notify();
    }

    fn select(&mut self, id: TaskId, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.new_task = None;
        self.confirm_discard = None;
        self.editing_headers = None;
        cx.notify();
    }
}

/// 任务停下时的信号。
fn ended_signal(
    status: &Status,
    output: &hs_m3u8_app::OutputSpec,
    result: Option<&Finished>,
) -> (Level, String) {
    match status {
        Status::Done => (
            Level::Info,
            result.map_or_else(
                || "完成".to_owned(),
                |r| format!("写出 {}", row::finished_text(output, r)),
            ),
        ),
        Status::Paused => (Level::Info, "已暂停，任务目录保留".to_owned()),
        Status::Failed(failure) => (Level::Error, failure.message.clone()),
        Status::Running | Status::Discarding => {
            unreachable!("任务服务只在任务停下后发结束事件，停下的状态不会是运行中或放弃没做完")
        }
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let show_list = !self.rows.is_empty() || self.new_task.is_some();
        let mut body = div().flex_1().min_h_0().flex().gap(px(GAP)).px(px(PAD));
        if show_list {
            body = body.child(self.render_list(now, cx));
        }
        body = match (&self.new_task, self.selected_row()) {
            (Some((view, _)), _) => body.child(view.clone()),
            (None, Some(row)) => {
                let (main, aside) = self.render_task(row, now, cx);
                body.child(main).child(aside)
            }
            (None, None) => body.child(self.render_standby(cx)),
        };
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::ground())
            .text_color(theme::text())
            .text_size(px(14.))
            .child(self.render_header(now))
            .child(body)
            .child(self.render_footer(cx))
            .child(self.render_notices(cx))
    }
}
