//! 主窗口：顶栏；左边一列任务小环；中间是选中任务的刻度环（新建时换成同心环，没有任务时是待机环）；右边是这个任务
//! 的信号；底部是粘贴栏；右上角是要用户关掉的提示。这里管状态、事件与操作，各块的画法在子模块里。

mod notices;
mod paste_bar;
mod row;
mod task_list;
mod task_view;
mod title_bar;

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, PromptButton, PromptLevel,
    Render, Styled as _, Subscription, Task, Window, div, px,
};
use hs_m3u8_app::{Draft, Event, Events, Finished, Service, Status, TaskId};
use hs_m3u8_core::{Stage, Url};

use self::row::{Disk, Present, Row};
use crate::links;
use crate::new_task::{NewTask, NewTaskEvent};
use crate::rate::Rate;
use crate::signals::Level;
use crate::theme;
use crate::ui::{GAP, PAD};

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
    /// 用户选了退出：等运行中的任务都停下就退出；期间不开始新的运行
    quitting: bool,
    /// 在服务里进行中的新建：退出要等它们（建成的随即暂停）
    creating: usize,
    _events: Task<()>,
    _ticker: Task<()>,
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
            quitting: false,
            creating: 0,
            _events: receiving,
            _ticker: ticker,
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
        match event {
            Event::Progress { id, progress } => {
                if let Some(row) = self.row_mut(id) {
                    row.rate
                        .update(Instant::now(), progress.received, progress.segments.done);
                    for (level, text) in crate::signals::between(row.progress.as_ref(), &progress) {
                        row.signal(level, text);
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

    /// 任务不在运行时：暂停与失败的读一次任务目录，完成的查一次输出在不在；放弃中的什么也不做（目录正在删）。
    fn after_stop(&mut self, id: TaskId, cx: &mut Context<Self>) {
        let Some(row) = self.row(id) else {
            return;
        };
        if row.discarding {
            return;
        }
        match &row.task.status {
            Status::Paused | Status::Failed(_) => self.inspect(id, false, cx),
            Status::Done => {
                let target = row.task.spec.output.options().target;
                let (mp4, hls) = (
                    target.mp4().map(PathBuf::from),
                    target.hls().map(PathBuf::from),
                );
                let checking = cx.background_executor().spawn(async move {
                    let mp4 = mp4.filter(|p| p.is_file());
                    let shown = mp4.clone().or_else(|| hls.filter(|p| p.exists()));
                    Present { mp4, shown }
                });
                cx.spawn(async move |this, cx| {
                    let present = checking.await;
                    // 查的期间窗口关掉时不再需要结果
                    let _closed = this.update(cx, |this, cx| {
                        if let Some(row) = this.row_mut(id) {
                            row.present = Some(present);
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            Status::Running => {}
        }
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

    /// 有任务在运行、继续、放弃或新建：退出要等它们。
    fn busy(&self) -> usize {
        self.creating
            + self
                .rows
                .iter()
                .filter(|r| r.task.status == Status::Running || !r.idle())
                .count()
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

    /// 读任务目录里已完成的内容；`then_resume` 时读完即继续，读到的片数作本次运行的起点（读不出时不估剩余时间）。
    /// 读完（及继续）后，选了退出、都已停下的即退出。
    fn inspect(&mut self, id: TaskId, then_resume: bool, cx: &mut Context<Self>) {
        let Some(reading) = self.row(id).map(|row| row.reading) else {
            return;
        };
        let handle = self.service.inspect(id);
        cx.spawn(async move |this, cx| {
            let result = handle.await;
            // 等待期间窗口关掉时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                let disk = match result {
                    Ok(Ok(saved)) => Disk::Read(saved),
                    Ok(Err(error)) => Disk::Unreadable(error.to_string()),
                    Err(join) => Disk::Unreadable(format!("读的过程中出错：{join}")),
                };
                // 读的期间任务又运行过、停下了的，这次读到的已过时，停下时另读了
                if let Some(row) = this.row_mut(id)
                    && row.reading == reading
                {
                    row.disk = disk;
                }
                if then_resume {
                    this.resume_now(id);
                }
                this.quit_if_idle(cx);
                cx.notify();
            });
        })
        .detach();
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

    fn probe(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(url) = links::http_url(text) else {
            self.input_error = Some("要 http 或 https 开头的链接".to_owned());
            cx.notify();
            return;
        };
        self.input_error = None;
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
            |this: &mut Self, view, event: &NewTaskEvent, window, cx| {
                match event {
                    NewTaskEvent::Create(draft) => {
                        this.create(view.clone(), (**draft).clone(), window, cx)
                    }
                    NewTaskEvent::Dismissed => this.new_task = None,
                    NewTaskEvent::FolderChanged(folder) => this.folder = folder.clone(),
                }
                cx.notify();
            },
        );
        self.new_task = Some((view, subscription));
        cx.notify();
    }

    /// 把 `url` 填进粘贴栏并探测。
    fn probe_url(&mut self, url: &Url, window: &mut Window, cx: &mut Context<Self>) {
        let text = url.to_string();
        self.paste
            .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
        self.probe(&text, window, cx);
    }

    /// 按新建视图里的选择新建任务；退出中不新建。新建要查磁盘，在服务的运行时里做，期间新建视图显示正在开始。
    fn create(
        &mut self,
        view: Entity<NewTask>,
        draft: Draft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quitting {
            view.update(cx, |view, cx| {
                view.failed("正在退出，不开始新任务".to_owned(), cx)
            });
            return;
        }
        let created_at = match SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
        {
            Some(seconds) => seconds,
            None => {
                view.update(cx, |view, cx| {
                    view.failed("系统时钟早于 1970 年，记不了任务的创建时间".to_owned(), cx)
                });
                return;
            }
        };
        view.update(cx, |view, cx| view.creating(cx));
        self.creating += 1;
        let handle = self.service.create(draft, created_at);
        cx.spawn_in(window, async move |this, cx| {
            let result = handle.await;
            // 等待期间窗口关掉时不再需要结果；任务照常在服务里开始，下次打开时在列表里
            let _closed = this.update_in(cx, |this, window, cx| {
                this.creating -= 1;
                match result {
                    Ok(Ok(task)) => this.started(&view, task, window, cx),
                    Ok(Err(error)) => this.create_failed(&view, error.to_string(), cx),
                    Err(join) => this.create_failed(&view, format!("新建中途出错：{join}"), cx),
                }
                this.quit_if_idle(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 新建视图 `view` 发起的任务建成了：加进列表。`view` 还开着（用户没切走）时关掉它、选中新任务、清空粘贴栏；
    /// 选了退出的随即暂停。
    fn started(
        &mut self,
        view: &Entity<NewTask>,
        task: hs_m3u8_app::Task,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = task.id;
        self.rows.push(Row::new(task, Instant::now()));
        if self.quitting {
            self.service.pause(id);
        }
        if self.showing(view) {
            self.new_task = None;
            self.selected = Some(id);
            self.paste
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// 新建失败：`view` 还开着时写在它上面，否则（用户已切走）提示。
    fn create_failed(&mut self, view: &Entity<NewTask>, error: String, cx: &mut Context<Self>) {
        if self.showing(view) {
            view.update(cx, |view, cx| view.failed(error, cx));
        } else {
            let url = view.read(cx).url().clone();
            self.notify(format!(
                "新建 {} 失败：{error}",
                hs_m3u8_core::bare_url(&url)
            ));
        }
    }

    /// 正显示着的新建视图是 `view`。
    fn showing(&self, view: &Entity<NewTask>) -> bool {
        self.new_task
            .as_ref()
            .is_some_and(|(shown, _)| shown == view)
    }

    fn select(&mut self, id: TaskId, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.new_task = None;
        self.confirm_discard = None;
        cx.notify();
    }

    /// 继续：先读出任务目录里已有的，作本次运行的起点，再开始；读的期间这一行不能再操作。
    fn resume(&mut self, id: TaskId, cx: &mut Context<Self>) {
        if self.quitting {
            self.notify("正在退出，不再继续任务".to_owned());
            cx.notify();
            return;
        }
        let Some(row) = self.row_mut(id) else {
            return;
        };
        if !row.idle() {
            return;
        }
        row.resuming = true;
        self.inspect(id, true, cx);
        cx.notify();
    }

    fn resume_now(&mut self, id: TaskId) {
        let title = self.title(id);
        let quitting = self.quitting;
        let Some(row) = self.row_mut(id) else {
            return;
        };
        row.resuming = false;
        if row.discarding {
            return;
        }
        if quitting {
            self.notify(format!("正在退出，「{title}」没有继续"));
            return;
        }
        let baseline = match &row.disk {
            Disk::Read(saved) => Some(saved.as_ref().map_or(0, |s| s.segments)),
            Disk::Unread | Disk::Unreadable(_) => None,
        };
        match self.service.resume(id) {
            Ok(()) => {
                let row = self.row_mut(id).expect("上面刚找到这一行");
                row.task.status = Status::Running;
                row.progress = None;
                row.rate = Rate::new(Instant::now(), baseline);
                let text = match baseline {
                    Some(n) if n > 0 => format!("继续：任务目录里已有 {n} 片"),
                    _ => "继续".to_owned(),
                };
                row.signal(Level::Info, text);
            }
            Err(error) => self.notify(format!("继续「{title}」失败：{error}")),
        }
    }

    /// 放弃任务：要删已下载内容的，第一次点只是要求确认。
    fn discard(&mut self, id: TaskId, confirmed: bool, cx: &mut Context<Self>) {
        if !confirmed {
            self.confirm_discard = Some(id);
            cx.notify();
            return;
        }
        self.confirm_discard = None;
        let title = self.title(id);
        if let Some(row) = self.row_mut(id) {
            row.discarding = true;
        }
        let handle = self.service.discard(id);
        cx.spawn(async move |this, cx| {
            let result = handle.await;
            // 等待期间窗口关掉时不再需要结果；放弃本身在服务里照常做完
            let _closed = this.update(cx, |this, cx| {
                let problems = match result {
                    Ok(Ok(leftovers)) => {
                        this.remove_row(id);
                        leftovers
                            .iter()
                            .map(|leftover| format!("放弃「{title}」时没删掉：{leftover}"))
                            .collect()
                    }
                    Ok(Err(error)) => vec![format!("放弃「{title}」失败：{error}")],
                    Err(join) => vec![format!("放弃「{title}」中途出错：{join}")],
                };
                if let Some(row) = this.row_mut(id) {
                    row.discarding = false;
                }
                if !problems.is_empty() && this.quitting {
                    this.cancel_quit("放弃没做干净，没有退出".to_owned());
                }
                for text in problems {
                    this.notify(text);
                }
                this.after_stop(id, cx);
                this.quit_if_idle(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 已完成的任务从列表移除：只删记录，输出归用户。
    fn forget(&mut self, id: TaskId, cx: &mut Context<Self>) {
        match self.service.forget(id) {
            Ok(()) => self.remove_row(id),
            Err(error) => {
                let title = self.title(id);
                self.notify(format!("移除「{title}」失败：{error}"));
            }
        }
        cx.notify();
    }

    /// 关窗前：有任务在运行时问一句。选了退出就暂停（或停止录制并写出）它们，等都停下再退出。
    /// 没有任务在运行时先把没写进存储的状态补写进去，补写不进时问要不要仍然退出。
    fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let busy = self.busy();
        if busy == 0 {
            let Err(error) = self.service.flush() else {
                return true;
            };
            let answer = window.prompt(
                PromptLevel::Warning,
                "有任务的状态没能写进存储",
                Some(&format!(
                    "现在退出，这些任务下次打开时显示为暂停。原因：{error}"
                )),
                &[PromptButton::new("仍然退出"), PromptButton::cancel("取消")],
                cx,
            );
            cx.spawn(async move |_, cx| {
                if let Ok(0) = answer.await {
                    cx.update(|cx| cx.quit());
                }
            })
            .detach();
            return false;
        }
        if self.quitting {
            // 已在等任务停下；右上角的提示里可以立即退出
            return false;
        }
        let recording = self
            .rows
            .iter()
            .any(|r| r.task.status == Status::Running && is_recording(r));
        // 回车选第一项，放能撤回的「暂停」；「取消」是取消按钮，Esc 即它
        let mut answers = vec![PromptButton::new("全部暂停并退出")];
        if recording {
            answers.push(PromptButton::new("停止录制、写出后退出"));
        }
        answers.push(PromptButton::cancel("取消"));
        let cancel = answers.len() - 1;
        let detail = if recording {
            "暂停的任务下次打开后可以继续；录制暂停期间直播的内容录不到。"
        } else {
            "暂停的任务下次打开后可以继续。"
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("有 {busy} 个任务在运行"),
            Some(detail),
            &answers,
            cx,
        );
        cx.spawn(async move |this, cx| {
            let Ok(choice) = answer.await else {
                return;
            };
            if choice == cancel {
                return;
            }
            let stop_recordings = recording && choice == 1;
            // 提示框开着期间视图已不在时没有要做的
            let _closed = this.update(cx, |this, cx| {
                this.quitting = true;
                for row in &this.rows {
                    if row.task.status != Status::Running {
                        continue;
                    }
                    if stop_recordings && is_recording(row) {
                        this.service.stop(row.task.id);
                    } else {
                        this.service.pause(row.task.id);
                    }
                }
                this.quit_if_idle(cx);
                cx.notify();
            });
        })
        .detach();
        false
    }

    /// 不退出了：失败与没删干净的东西留给用户看；`why` 写进提示。
    fn cancel_quit(&mut self, why: String) {
        self.quitting = false;
        self.notify(why);
    }

    /// 选了退出、任务都停下（继续、放弃与新建也做完）时，把没写进存储的状态补写进去后退出；补写不进时不退出。
    fn quit_if_idle(&mut self, cx: &mut Context<Self>) {
        if !self.quitting || self.busy() > 0 {
            return;
        }
        match self.service.flush() {
            Ok(()) => cx.quit(),
            Err(error) => self.cancel_quit(format!(
                "有任务的状态没能写进存储（{error}），没有退出；再关窗口时可以选择仍然退出"
            )),
        }
    }
}

/// 正在录制：只有到了录制阶段的才能停止并写出，其余的停止不起作用，要暂停。
fn is_recording(row: &Row) -> bool {
    row.progress
        .as_ref()
        .is_some_and(|p| p.stage == Stage::Recording)
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
        Status::Failed(message) => (Level::Error, message.clone()),
        Status::Running => unreachable!("任务服务只在任务停下后发结束事件"),
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
            (Some((view, _)), _) => body.child(div().flex_1().flex().child(view.clone())),
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
