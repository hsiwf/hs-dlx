//! 与窗口无关的任务状态：任务服务、列表里的各行与它们这次运行收到的进度、要用户看的提示、退出进行到哪一步、保存目录
//! 所在盘的可用空间、后打开的实例转交来的链接。对任务的操作在这里发起并收尾（读任务目录、放弃、新建的结果都在这里
//! 落进各行），窗口只发起与显示，窗口不在时照常进行。

mod operations;
mod quit;
mod row;

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::{Context, EventEmitter, Subscription, Task};
use hs_m3u8_app::{Event, Events, Finished, Service, Status, TaskId};
use hs_m3u8_core::Stage;
use hs_m3u8_instance::{Handoff, Handoffs, Received};

pub use self::operations::resume_label;
pub use self::quit::Quit;
pub use self::row::{Look, Row, finished_text, look};
use crate::signals::Level;
use crate::wait::within;

/// 多久读一次保存目录所在盘的可用空间。
const FREE_SPACE_INTERVAL: Duration = Duration::from_secs(10);

/// 读一次可用空间最多等多久：网络盘掉线时读会卡住。
const FREE_SPACE_TIMEOUT: Duration = Duration::from_secs(3);

/// 保存目录 `folder` 所在盘对本用户可用的字节数；目录不在（被删、所在的盘没接上）或读不出时为原因（顶栏照原样显示）。
fn free_space(folder: &std::path::Path) -> Result<u64, String> {
    match folder.try_exists() {
        Ok(true) => {
            fs4::available_space(folder).map_err(|error| format!("读不出可用空间：{error}"))
        }
        Ok(false) => Err("保存目录不在".to_owned()),
        Err(error) => Err(format!("读不出可用空间：{error}")),
    }
}

/// 要窗口处理的事。
pub enum TasksEvent {
    /// 后打开的实例转交来的（已接手）：窗口到前台，有链接就探测
    HandedOver(Handoff),
}

/// 此刻在运行的任务合起来的样子（顶栏与托盘用）。
pub struct Activity {
    pub running: usize,
    /// 合计下载速度（字节每秒）
    pub speed: f64,
    /// 在下的点播的平均进度（0 到 1）；没有在下的点播时为 None
    pub progress: Option<f32>,
    /// 有在录的直播
    pub recording: bool,
}

impl Activity {
    /// 「2 个进行中 · ↓ 18.4 MB/s」；没有在运行的任务时为 None。
    pub fn summary(&self) -> Option<String> {
        (self.running > 0).then(|| {
            format!(
                "{} 个进行中 · ↓ {}/s",
                self.running,
                crate::format::bytes(self.speed as u64)
            )
        })
    }
}

/// 一条要用户看到、关掉才消失的提示（操作没成、没删干净的东西）。
pub struct Notice {
    pub id: u64,
    pub text: String,
}

pub struct Tasks {
    service: Rc<Service>,
    /// 按新建先后
    rows: Vec<Row>,
    /// 新建任务默认保存到的目录
    folder: PathBuf,
    notices: Vec<Notice>,
    next_notice: u64,
    quit: Quit,
    /// 在服务里进行中的新建：退出要等它们（建成的随即暂停）
    creating: usize,
    /// 新建期间收到的、列表里还没有的任务的结束事件：新建的结果与任务的事件走两条路，结束可能先到；加进那一行时再处理
    early: Vec<Event>,
    /// 保存目录所在盘的可用空间（字节）：还没读到时为 None，读不出、目录不在或读超时时为原因
    free_space: Option<Result<u64, String>>,
    _events: Task<()>,
    _handoffs: Task<()>,
    _ticker: Task<()>,
    _free_space: Task<()>,
    _quitting: Subscription,
}

impl EventEmitter<TasksEvent> for Tasks {}

impl Tasks {
    /// `handoffs` 为后打开的实例转交来的（见 hs-m3u8-instance），收不了转交时为原因（提示一次，之后打开的实例各自
    /// 报错）。
    pub fn new(
        service: Service,
        mut events: Events,
        folder: PathBuf,
        handoffs: Result<Handoffs, String>,
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
        // 应用真要退出了（问过之后，或不经询问的退出，如别的平台注销）：停下所有任务、补写状态（最多等 2 秒）
        let quitting = cx.on_app_quit(|this: &mut Self, _| {
            // 没有窗口能显示了；补写不进的下次打开时按暂停处理（见 Service::close）
            let _unsaved = this.service.close();
            async {}
        });
        let handing = match handoffs {
            Ok(mut handoffs) => cx.spawn(async move |this, cx| {
                while let Some(received) = handoffs.recv().await {
                    if this
                        .update(cx, |this, cx| this.receive(received, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }),
            Err(reason) => {
                notices.push(Notice {
                    id: notices.len() as u64,
                    text: format!("收不了再次打开应用时转交的链接：{reason}"),
                });
                Task::ready(())
            }
        };
        let receiving = cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this.update(cx, |this, cx| this.apply(event, cx)).is_err() {
                    break;
                }
            }
        });
        // 速度按时间窗算，没有新进度时也要随时间降下来：有任务在运行时每秒通知一次
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
        let mut tasks = Tasks {
            service,
            rows,
            folder,
            next_notice: notices.len() as u64,
            notices,
            quit: Quit::Staying,
            creating: 0,
            early: Vec::new(),
            free_space: None,
            _events: receiving,
            _handoffs: handing,
            _ticker: ticker,
            _free_space: Task::ready(()),
            _quitting: quitting,
        };
        let ids: Vec<TaskId> = tasks.rows.iter().map(|r| r.task.id).collect();
        for id in ids {
            tasks.after_stop(id, cx);
        }
        tasks.watch_free_space(cx);
        tasks
    }

    /// 再次打开应用时转交来的：已决定退出时不接手（交来的一方告诉用户正在退出）；否则接手，等着退出的、正在问要不要
    /// 退出的不再退出，交给窗口（[`TasksEvent::HandedOver`]）。一次没收成的转交提示原因。
    fn receive(&mut self, received: Received, cx: &mut Context<Self>) {
        match received {
            Ok(delivery) if self.quit == Quit::Leaving => delivery.refuse_leaving(),
            Ok(delivery) => {
                // 交出的一方已不等（2 秒没接手）时它已把链接告诉用户，这里不再处理
                let Some(handoff) = delivery.accept() else {
                    return;
                };
                if let Quit::Asking { reopened } = &mut self.quit {
                    *reopened = true;
                }
                if self.quitting() {
                    self.cancel_quit("再次打开了应用，没有退出".to_owned());
                }
                cx.emit(TasksEvent::HandedOver(handoff));
            }
            Err(reason) => self.notify(format!("再次打开应用时转交的链接没收到：{reason}")),
        }
        cx.notify();
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
                if self.quitting() {
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
        if self.quitting() && (failed || !problems.is_empty()) {
            self.cancel_quit(format!("「{title}」停下时有要处理的，没有退出"));
        }
        for text in problems {
            self.notify(text);
        }
        self.after_stop(id, cx);
        self.quit_if_idle(cx);
    }

    /// （重新）开始读保存目录所在盘的可用空间：在后台读，每 10 秒一次。一次读超过 3 秒就先报超时，等它读完再接着
    /// 按间隔读，不叠着发起新的；换了保存目录时调一次，立即按新目录读。
    fn watch_free_space(&mut self, cx: &mut Context<Self>) {
        let folder = self.folder.clone();
        self._free_space = cx.spawn(async move |this, cx| {
            loop {
                let target = folder.clone();
                let mut read = std::pin::pin!(
                    cx.background_executor()
                        .spawn(async move { free_space(&target) })
                );
                let early = within(
                    read.as_mut(),
                    cx.background_executor().timer(FREE_SPACE_TIMEOUT),
                )
                .await;
                let value = match early {
                    Some(value) => value,
                    None => {
                        let late = Err(format!(
                            "可用空间读了 {} 秒没有结果",
                            FREE_SPACE_TIMEOUT.as_secs()
                        ));
                        let alive = this.update(cx, |this, cx| {
                            this.free_space = Some(late);
                            cx.notify();
                        });
                        if alive.is_err() {
                            break;
                        }
                        read.await
                    }
                };
                let alive = this.update(cx, |this, cx| {
                    this.free_space = Some(value);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(FREE_SPACE_INTERVAL).await;
            }
        });
    }

    /// 此刻在运行的任务合起来的样子；速度按 `now` 时的时间窗算。
    pub fn activity(&self, now: Instant) -> Activity {
        let running: Vec<&Row> = self
            .rows
            .iter()
            .filter(|row| row.task.status == Status::Running)
            .collect();
        let downloading: Vec<f32> = running
            .iter()
            .filter(|row| !row.recording())
            .map(|row| look(row, now).fraction)
            .collect();
        Activity {
            running: running.len(),
            speed: running
                .iter()
                .map(|row| row.rate.bytes_per_second(now))
                .sum(),
            progress: (!downloading.is_empty())
                .then(|| downloading.iter().sum::<f32>() / downloading.len() as f32),
            recording: running.iter().any(|row| row.recording()),
        }
    }

    pub fn service(&self) -> &Rc<Service> {
        &self.service
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn row(&self, id: TaskId) -> Option<&Row> {
        self.rows.iter().find(|row| row.task.id == id)
    }

    fn row_mut(&mut self, id: TaskId) -> Option<&mut Row> {
        self.rows.iter_mut().find(|row| row.task.id == id)
    }

    /// 列表里的名字；行已不在（放弃做完）时写编号。
    pub fn title(&self, id: TaskId) -> String {
        self.row(id)
            .map_or_else(|| id.to_string(), |r| r.task.spec.title.clone())
    }

    pub fn folder(&self) -> &PathBuf {
        &self.folder
    }

    /// 新建任务默认保存到的目录换了（已存进设置）：按新目录读可用空间。
    pub fn set_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.folder = folder;
        self.watch_free_space(cx);
        cx.notify();
    }

    pub fn free_space(&self) -> Option<&Result<u64, String>> {
        self.free_space.as_ref()
    }

    pub fn notices(&self) -> &[Notice] {
        &self.notices
    }

    /// 加一条提示（调用方随后通知窗口重画）。
    pub fn notify(&mut self, text: String) {
        self.notices.push(Notice {
            id: self.next_notice,
            text,
        });
        self.next_notice += 1;
    }

    pub fn dismiss_notice(&mut self, id: u64, cx: &mut Context<Self>) {
        self.notices.retain(|n| n.id != id);
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
                |r| format!("写出 {}", finished_text(output, r)),
            ),
        ),
        Status::Paused => (Level::Info, "已暂停，任务目录保留".to_owned()),
        Status::Failed(failure) => (Level::Error, failure.message.clone()),
        Status::Running | Status::DiscardUnfinished => {
            unreachable!("任务服务只在任务停下后发结束事件，停下的状态不会是运行中或放弃没做完")
        }
    }
}
