//! 任务服务：持有核心的 [`Engine`] 与它的 tokio 运行时，按存储里的任务开始、暂停、继续、停止与放弃，把节流后的
//! 进度与结束状态经事件通道交给界面。
//!
//! 服务在运行时里起的任务 panic 即不变量被破坏，服务不兜：由应用决定（桌面应用直接中止进程），存储里还标着运行中的
//! 任务下次打开时按暂停处理。

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use hs_m3u8_core::hls::Preference;
use hs_m3u8_core::remux::Shape;
use hs_m3u8_core::{
    CancellationToken, Engine, Job, JobControl, JobRequest, Leftover, LeftoverKind, Output, Probe,
    Progress, Saved, Source, Stage, Unsettled,
};
use rusqlite::Transaction;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;

use crate::preference::StoredPreference;
use crate::store::{self, Store};
use crate::{Draft, Error, Finished, Status, Task, TaskId, TaskSpec, naming};

/// 同一任务两次进度事件之间至少隔多久：界面每秒最多重画十次进度。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// 读任务目录最多等多久：网络盘没有响应时不让调用方一直等。
const INSPECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 关闭服务时，等取消了的任务停下并写进结束状态、等运行时的阻塞线程，各最多等多久。
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// 交给界面的事件。
#[derive(Debug)]
pub enum Event {
    /// 运行中任务的进度快照；同一任务两次之间至少隔 100 毫秒，进度没变时不发
    Progress { id: TaskId, progress: Box<Progress> },
    /// 任务停下。`result` 在状态为 [`Status::Done`] 时为写出的结果，`total` 同 [`Task::total`]，`leftovers` 为完成时
    /// 没删掉的东西（被替换的旧输出、没删干净的任务目录等，输出不受影响）。`unsaved` 为 Some 时状态没能写进存储：服务
    /// 记着它，下一次写存储（任何操作或 [`Service::flush`]）时补写，[`Service::tasks`] 也按它返回；补写之前服务关闭
    /// 而补写不进的，下次打开时按暂停处理
    Ended {
        id: TaskId,
        status: Status,
        result: Option<Finished>,
        total: Option<usize>,
        leftovers: Vec<Leftover>,
        unsaved: Option<Error>,
    },
    /// 运行中记点播的总片数时存储写不进；任务照常运行，停下时随结束状态再写一次
    TotalUnsaved { id: TaskId, error: Error },
    /// 新建任务时存储写不进，已开始的任务取消了，但磁盘上留下了东西：没删干净的任务目录，或取消前已写出的输出。它们
    /// 不在任何任务名下，要用户处理
    Orphaned { paths: Vec<PathBuf>, reason: String },
}

/// 事件的接收端，在任何执行器上都可等待。
pub type Events = mpsc::UnboundedReceiver<Event>;

/// 一个正在运行的任务。
struct Running {
    control: JobControl,
    /// 停下（状态已写进存储或记为待补写、已从运行表里去掉）时变为 true
    stopped: watch::Receiver<bool>,
}

/// 任务停下时要写进存储的。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ending {
    status: Status,
    result: Option<Finished>,
    /// 点播的总片数；None 时不改存储里的
    total: Option<usize>,
}

impl Ending {
    /// 把还没写进存储的结束状态叠到从存储读出的 `task` 上。
    fn overlay(&self, task: &mut Task) {
        task.status = self.status.clone();
        task.result.clone_from(&self.result);
        if self.total.is_some() {
            task.total = self.total;
        }
    }
}

/// 服务的状态，运行时里的任务与界面共用。锁的顺序：`creating` → `store` → 其余各把。
struct Shared {
    engine: Engine,
    store: Mutex<Store>,
    /// 新建任务逐个进行：取名先读存储、再查磁盘（锁外，网络盘上可能慢）、最后插入，两次新建交错会取到同一个名字
    creating: Mutex<()>,
    /// 运行中的任务；登记与查询放弃中都在 `store` 锁内，继续与放弃不会交错
    running: Mutex<HashMap<TaskId, Running>>,
    /// 正在放弃的任务：放弃做完（成功或失败）之前不能继续或移除
    discarding: Mutex<HashSet<TaskId>>,
    /// 停下了、但状态没能写进存储的任务：下一次写存储时补写
    unsaved: Mutex<HashMap<TaskId, Ending>>,
    /// 服务在运行时里起的收尾任务（运行中任务的收尾、存储写不进时不要了的任务的收尾）；关闭时等它们
    tasks: TaskTracker,
    events: mpsc::UnboundedSender<Event>,
}

/// 任务服务。不能在 tokio 运行时里释放：释放时要在自己的运行时上等任务停下（见 [`Service::close`]）。
pub struct Service {
    shared: Arc<Shared>,
    /// 只在释放时取走（释放要按值关闭运行时）
    runtime: Option<Runtime>,
}

impl Service {
    /// 用 `store` 建服务，同时最多 `max_requests` 个请求（见 [`Engine::new`]）；返回服务与事件的接收端。上次异常退出时
    /// 还标着运行中的任务改为暂停。
    pub fn new(mut store: Store, max_requests: NonZeroUsize) -> Result<(Service, Events), Error> {
        let tx = store.transaction()?;
        store::pause_interrupted(&tx)?;
        tx.commit()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("hs-m3u8-engine")
            .build()
            .map_err(|cause| Error::Io {
                action: "启动",
                path: "tokio 运行时".into(),
                cause,
            })?;
        let engine = {
            let _entered = runtime.enter();
            Engine::new(max_requests)
        };
        let (events, receiver) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            engine,
            store: Mutex::new(store),
            creating: Mutex::new(()),
            running: Mutex::new(HashMap::new()),
            discarding: Mutex::new(HashSet::new()),
            unsaved: Mutex::new(HashMap::new()),
            tasks: TaskTracker::new(),
            events,
        });
        let service = Service {
            shared,
            runtime: Some(runtime),
        };
        Ok((service, receiver))
    }

    fn runtime(&self) -> &Runtime {
        self.runtime.as_ref().expect("运行时只在释放时取走")
    }

    /// 全部任务，按新建先后；停下了而状态还没补写进存储的，按停下时的状态。
    pub fn tasks(&self) -> Result<Vec<Task>, Error> {
        let mut store = lock(&self.shared.store);
        let tx = store.transaction()?;
        let mut tasks = store::list(&tx)?;
        let unsaved = lock(&self.shared.unsaved);
        for task in &mut tasks {
            if let Some(ending) = unsaved.get(&task.id) {
                ending.overlay(task);
            }
        }
        Ok(tasks)
    }

    /// 有停下了而状态还没写进存储的任务。
    pub fn has_unsaved(&self) -> bool {
        !lock(&self.shared.unsaved).is_empty()
    }

    /// 把停下了而状态还没写进存储的补写进去。
    pub fn flush(&self) -> Result<(), Error> {
        self.shared.write(|_| Ok(()))
    }

    /// 退出前停下所有任务：取消运行中的任务，最多等 2 秒让它们与别的收尾任务停下、写进结束状态，
    /// 再补写没写进去的，返回补写的结果。没来得及停下的与补写不进的，存储里还是运行中，下次打开时按暂停处理。之后服务
    /// 照常可用。不能在 tokio 运行时里调用。
    pub fn close(&self) -> Result<(), Error> {
        for running in lock(&self.shared.running).values() {
            running.control.cancel();
        }
        let tasks = &self.shared.tasks;
        tasks.close();
        let _timed_out = self
            .runtime()
            .block_on(async { tokio::time::timeout(CLOSE_TIMEOUT, tasks.wait()).await });
        tasks.reopen();
        self.flush()
    }

    /// 探测来源；返回的句柄在任何执行器上都可等待，丢弃它不取消探测。
    pub fn probe(&self, source: Source) -> JoinHandle<Result<Probe, hs_m3u8_core::Error>> {
        let engine = self.shared.engine.clone();
        self.runtime()
            .spawn(async move { engine.probe(&source).await })
    }

    /// 按 `draft` 新建任务并开始，返回存进存储的任务；`created_at` 为新建时间（Unix 秒）。输出放在 `draft.folder` 里，
    /// 主名由地址得出（见 [`crate::title_of`]），避开磁盘上已有的与存储里各任务占用的输出与任务目录（不分大小写与
    /// Unicode 写法），被占用时加编号；任务的标题即这个主名，任务目录为核心的默认位置。`draft.folder` 不是绝对路径、
    /// 选轨偏好存不了时报 [`Error::InvalidInput`]。取名要查磁盘（网络盘上可能慢），在运行时的阻塞线程上、存储锁外做，
    /// 几次新建逐个进行；返回的句柄在任何执行器上都可等待。存储写不进时什么也不留：已开始的任务取消，它建出的任务目录
    /// 删掉，磁盘上还留下东西时发 [`Event::Orphaned`]。
    pub fn create(&self, draft: Draft, created_at: i64) -> JoinHandle<Result<Task, Error>> {
        let shared = self.shared.clone();
        self.runtime()
            .spawn_blocking(move || shared.create(draft, created_at))
    }

    /// 继续暂停或失败的任务：照新建时的请求再开始，任务目录里已完成的分片不重下。已完成的报 [`Error::AlreadyDone`]，
    /// 没有自己的任务目录的报 [`Error::NoWorkDir`]。存储写不进时任务已开始，随即取消，停下后照常发 [`Event::Ended`]。
    pub fn resume(&self, id: TaskId) -> Result<(), Error> {
        let _entered = self.runtime().enter();
        self.shared.resume(id)
    }

    /// 任务目录里已完成的内容（见 [`Engine::inspect`]）：任务不在运行时显示已下了多少、继续前算出续传的起点。任务没有
    /// 自己的任务目录时为 None；10 秒内读不完报 [`Error::TimedOut`]。返回的句柄在任何执行器上都
    /// 可等待。
    pub fn inspect(&self, id: TaskId) -> JoinHandle<Result<Option<Saved>, Error>> {
        let shared = self.shared.clone();
        self.runtime().spawn(async move {
            let Some(work_dir) = shared.get(id)?.work_dir else {
                return Ok(None);
            };
            tokio::time::timeout(INSPECT_TIMEOUT, shared.engine.inspect(&work_dir))
                .await
                .map_err(|_| Error::TimedOut {
                    id,
                    after: INSPECT_TIMEOUT,
                })?
                .map_err(Error::from)
        })
    }

    /// 只从存储里删掉任务，不碰磁盘：用于已完成的（任务目录已在完成时删掉，输出归用户）与没有自己的任务目录的。别的报
    /// [`Error::NotFinished`]：它们的任务目录要用 [`Service::discard`] 一起删。
    pub fn forget(&self, id: TaskId) -> Result<(), Error> {
        self.shared.write(|tx| {
            if lock(&self.shared.discarding).contains(&id) {
                return Err(Error::Discarding(id));
            }
            let task = store::get(tx, id)?.ok_or(Error::NotFound(id))?;
            if task.status != Status::Done && task.work_dir.is_some() {
                return Err(Error::NotFinished(id));
            }
            store::delete(tx, id)
        })
    }

    /// 暂停：取消运行中的任务，保留任务目录；停下后状态为 [`Status::Paused`]。没在运行时什么也不做。
    pub fn pause(&self, id: TaskId) {
        if let Some(running) = lock(&self.shared.running).get(&id) {
            running.control.cancel();
        }
    }

    /// 停止录制：把已列出的分片下完后写出（见 [`JobControl::stop`]）。没在运行时、或任务是点播时什么也不做。
    pub fn stop(&self, id: TaskId) {
        if let Some(running) = lock(&self.shared.running).get(&id) {
            running.control.stop();
        }
    }

    /// 放弃任务：在运行就先取消并等它停下，再删掉任务目录（见 [`Engine::discard`]）与存储里的记录；返回留下的东西。
    /// 任务目录没删干净时记录留着，报 [`Error::WorkDirLeft`]，处理后可再放弃。做完之前任务记为正在放弃，继续与移除
    /// 都报 [`Error::Discarding`]，再放弃一次同样报这个错误。返回的句柄在任何执行器上都可等待。
    pub fn discard(&self, id: TaskId) -> JoinHandle<Result<Vec<Leftover>, Error>> {
        let stopped = {
            let _store = lock(&self.shared.store);
            if !lock(&self.shared.discarding).insert(id) {
                return self
                    .runtime()
                    .spawn(async move { Err(Error::Discarding(id)) });
            }
            lock(&self.shared.running).get(&id).map(|running| {
                running.control.cancel();
                running.stopped.clone()
            })
        };
        let guard = DiscardGuard {
            id,
            shared: self.shared.clone(),
        };
        self.runtime().spawn(async move {
            let result = run_discard(id, &guard.shared, stopped).await;
            drop(guard);
            result
        })
    }
}

impl Drop for Service {
    /// 先 [`Service::close`]，再关掉运行时（阻塞线程最多等 2 秒）；没停下的任务随运行时丢弃。
    fn drop(&mut self) {
        // 释放时没有调用方能处理补写的错误，结果见 close 的说明；要知道结果的调用方先调 close
        let _unsaved = self.close();
        let runtime = self.runtime.take().expect("运行时只在这里取走");
        runtime.shutdown_timeout(CLOSE_TIMEOUT);
    }
}

/// 放弃中的登记：放弃做完或中途 panic 时都从放弃中集合里去掉。
struct DiscardGuard {
    id: TaskId,
    shared: Arc<Shared>,
}

impl Drop for DiscardGuard {
    fn drop(&mut self) {
        lock(&self.shared.discarding).remove(&self.id);
    }
}

/// [`Service::discard`] 的步骤：等任务停下、删任务目录、删存储里的记录。
async fn run_discard(
    id: TaskId,
    shared: &Shared,
    stopped: Option<watch::Receiver<bool>>,
) -> Result<Vec<Leftover>, Error> {
    if let Some(mut stopped) = stopped {
        // 运行登记释放时先置为 true 再丢弃发送端，等到值或发送端已丢弃都说明任务已停下
        let _stopped = stopped.wait_for(|done| *done).await;
    }
    let leftovers = match shared.get(id)?.work_dir {
        Some(work_dir) => {
            shared
                .engine
                .discard(&work_dir, Unsettled::Fail, &CancellationToken::new())
                .await?
        }
        None => Vec::new(),
    };
    if leftovers
        .iter()
        .any(|leftover| leftover.kind == LeftoverKind::WorkDir)
    {
        return Err(Error::WorkDirLeft { id, leftovers });
    }
    shared.write(|tx| store::delete(tx, id))?;
    Ok(leftovers)
}

/// 运行中任务的登记：任务停下（或收尾的任务 panic）时从运行表里去掉，并告诉等它停下的放弃。
struct RunningGuard {
    id: TaskId,
    shared: Arc<Shared>,
    stopped: watch::Sender<bool>,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        lock(&self.shared.running).remove(&self.id);
        self.stopped.send_replace(true);
    }
}

impl Shared {
    /// 在一个事务里先补写没写进去的结束状态，再做 `f`，然后提交；补写进去的从待补写里去掉。
    fn write<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T, Error>) -> Result<T, Error> {
        let mut store = lock(&self.store);
        let tx = store.transaction()?;
        let applied = self.apply_unsaved(&tx)?;
        let value = f(&tx)?;
        tx.commit()?;
        self.clear_unsaved(applied);
        Ok(value)
    }

    /// 存储里的任务 `id`；没有时报 [`Error::NotFound`]。
    fn get(&self, id: TaskId) -> Result<Task, Error> {
        let mut store = lock(&self.store);
        let tx = store.transaction()?;
        store::get(&tx, id)?.ok_or(Error::NotFound(id))
    }

    /// 把待补写的结束状态写进事务 `tx`，返回写了的；任务已不在存储里的不写（它被放弃或移除了）。
    fn apply_unsaved(&self, tx: &Transaction<'_>) -> Result<Vec<(TaskId, Ending)>, Error> {
        let unsaved: Vec<(TaskId, Ending)> = lock(&self.unsaved)
            .iter()
            .map(|(id, ending)| (*id, ending.clone()))
            .collect();
        for (id, ending) in &unsaved {
            match save_ending(tx, *id, ending) {
                Ok(()) | Err(Error::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(unsaved)
    }

    /// 已提交的补写从待补写里去掉；其间同一任务又记了新的结束状态时留着新的。调用方须持着 `store` 锁，与提交之间
    /// 没有别的写入。
    fn clear_unsaved(&self, applied: Vec<(TaskId, Ending)>) {
        let mut unsaved = lock(&self.unsaved);
        for (id, ending) in applied {
            if unsaved.get(&id) == Some(&ending) {
                unsaved.remove(&id);
            }
        }
    }

    fn create(self: &Arc<Self>, draft: Draft, created_at: i64) -> Result<Task, Error> {
        if !draft.folder.is_absolute() {
            return Err(Error::InvalidInput(format!(
                "保存目录不是绝对路径：{}",
                draft.folder.display()
            )));
        }
        let preference = StoredPreference::try_from(&draft.preference)?;
        let _creating = lock(&self.creating);
        let taken = {
            let mut store = lock(&self.store);
            let tx = store.transaction()?;
            store::taken_paths(&tx)?
        };
        let (title, output) = naming::output_for(
            &draft.folder,
            &naming::title_of(&draft.url),
            draft.output,
            &taken,
        )?;
        let work_dir = output.options().resolved_work_dir()?;
        let spec = TaskSpec {
            title,
            url: draft.url,
            preference,
            headers: draft.headers,
            output,
            live: draft.live,
        };
        let mut store = lock(&self.store);
        let tx = store.transaction()?;
        let applied = self.apply_unsaved(&tx)?;
        let id = store::insert(&tx, &spec, &work_dir, created_at)?;
        let task = store::get(&tx, id)?.expect("同一事务里刚插入的行读得到");
        let job = self.engine.start(request(&task)?)?;
        if let Err(error) = tx.commit() {
            drop(store);
            self.abandon(job, work_dir);
            return Err(Error::Store(error));
        }
        self.clear_unsaved(applied);
        self.track(id, job, None);
        Ok(task)
    }

    fn resume(self: &Arc<Self>, id: TaskId) -> Result<(), Error> {
        let mut store = lock(&self.store);
        if lock(&self.running).contains_key(&id) {
            return Err(Error::Running(id));
        }
        if lock(&self.discarding).contains(&id) {
            return Err(Error::Discarding(id));
        }
        let tx = store.transaction()?;
        let applied = self.apply_unsaved(&tx)?;
        let task = store::get(&tx, id)?.ok_or(Error::NotFound(id))?;
        if task.status == Status::Done {
            return Err(Error::AlreadyDone(id));
        }
        let request = request(&task)?;
        store::set_status(&tx, id, &Status::Running)?;
        let job = self.engine.start(request)?;
        let committed = tx.commit();
        match &committed {
            Ok(()) => self.clear_unsaved(applied),
            // 任务目录是这个任务原有的：照常登记，停下后按暂停写存储（同样写不进时记为待补写）
            Err(_) => job.control().cancel(),
        }
        self.track(id, job, task.total);
        drop(store);
        committed.map_err(Error::Store)
    }

    /// 新建时存储写不进而不要的任务：取消，等它停下，再删掉它建出的任务目录；磁盘上还留下东西时发
    /// [`Event::Orphaned`]。
    fn abandon(self: &Arc<Self>, job: Job, work_dir: PathBuf) {
        job.control().cancel();
        let shared = self.clone();
        self.tasks.spawn(async move {
            let (paths, reason) = match job.wait().await {
                Ok(output) => (
                    written(&output),
                    "新建的任务没能记进存储，取消前已写出输出".to_owned(),
                ),
                // 已取消，错误只能是取消或取消前就出的错，任务都已不要
                Err(_) => match shared
                    .engine
                    .discard(&work_dir, Unsettled::Fail, &CancellationToken::new())
                    .await
                {
                    Ok(leftovers) if leftovers.is_empty() => return,
                    Ok(leftovers) => (
                        vec![work_dir],
                        leftovers
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join("；"),
                    ),
                    Err(error) => (vec![work_dir], error.to_string()),
                },
            };
            // 接收端随界面关闭而丢弃时没有谁能处理
            let _closed = shared.events.send(Event::Orphaned { paths, reason });
        });
    }

    /// 登记运行中的任务，转发它的进度，等它停下后把状态写进存储并发出 [`Event::Ended`]。`total` 为存储里记着的点播
    /// 总片数；下载开始后的总片数与它不同时记下。调用方须在运行时里。
    fn track(self: &Arc<Self>, id: TaskId, job: Job, total: Option<usize>) {
        let control = job.control();
        let (stopped_tx, stopped) = watch::channel(false);
        lock(&self.running).insert(
            id,
            Running {
                control: control.clone(),
                stopped,
            },
        );
        let guard = RunningGuard {
            id,
            shared: self.clone(),
            stopped: stopped_tx,
        };
        let shared = self.clone();
        self.tasks.spawn(async move {
            let done = CancellationToken::new();
            let forwarding = tokio::spawn(forward(
                shared.clone(),
                id,
                control.clone(),
                total,
                done.clone(),
            ));
            let result = job.wait().await;
            done.cancel();
            let forwarded = forwarding
                .await
                .expect("转发进度的任务不会 panic，运行时关闭时本任务也一起丢弃");
            // 转发按节流取样，下得快时可能没见过下载阶段；按最后的进度再定一次点播的总片数
            let last = control.progress().borrow().clone();
            let vod = match &result {
                Ok(output) => output.live.is_none(),
                Err(_) => last.stage == Stage::Downloading,
            };
            let total = if vod && last.segments.total > 0 {
                Some(last.segments.total)
            } else {
                forwarded
            };
            let (status, result, leftovers) = match result {
                Ok(output) => {
                    let result = finished_result(&output, last.duration);
                    (Status::Done, Some(result), output.leftovers)
                }
                Err(hs_m3u8_core::Error::Cancelled) => (Status::Paused, None, Vec::new()),
                Err(error) => (Status::Failed(error.to_string()), None, Vec::new()),
            };
            let ending = Ending {
                status,
                result,
                total,
            };
            let saved = shared.write(|tx| save_ending(tx, id, &ending));
            if saved.is_err() {
                lock(&shared.unsaved).insert(id, ending.clone());
            }
            drop(guard);
            let Ending {
                status,
                result,
                total,
            } = ending;
            // 接收端随界面关闭而丢弃时没有谁要这个事件；状态已写进存储或留着待补写
            let _closed = shared.events.send(Event::Ended {
                id,
                status,
                result,
                total,
                leftovers,
                unsaved: saved.err(),
            });
        });
    }
}

/// 转发任务 `id` 的进度，同时记下点播的总片数，直到 `done` 取消或进度不再更新；返回最后知道的总片数（开始时为
/// `total`）。
async fn forward(
    shared: Arc<Shared>,
    id: TaskId,
    control: JobControl,
    mut total: Option<usize>,
    done: CancellationToken,
) -> Option<usize> {
    let mut progress = control.progress();
    loop {
        match done.run_until_cancelled(progress.changed()).await {
            Some(Ok(())) => {}
            None | Some(Err(_)) => return total,
        }
        let snapshot = progress.borrow_and_update().clone();
        // 点播的总片数在下载开始时就定了；直播与准备阶段不记
        let count = snapshot.segments.total;
        if snapshot.stage == Stage::Downloading && count > 0 && total != Some(count) {
            total = Some(count);
            if let Err(error) = shared.write(|tx| store::set_total(tx, id, count)) {
                // 接收端随界面关闭而丢弃时没有谁能处理；停下时随结束状态再写一次
                let _closed = shared.events.send(Event::TotalUnsaved { id, error });
            }
        }
        let _closed = shared.events.send(Event::Progress {
            id,
            progress: Box::new(snapshot),
        });
        if done
            .run_until_cancelled(tokio::time::sleep(PROGRESS_INTERVAL))
            .await
            .is_none()
        {
            return total;
        }
    }
}

/// 输出写出的路径：MP4 与本地 HLS 目录。
fn written(output: &Output) -> Vec<PathBuf> {
    output
        .mp4
        .iter()
        .map(|mp4| mp4.path.clone())
        .chain(output.hls.clone())
        .collect()
}

/// 完成时要记下的结果：MP4 里视频流的时长与宽高，没有时时长取 `declared`（第 0 条轨分片的声明时长之和，为 0 时不记）。
fn finished_result(output: &Output, declared: Duration) -> Finished {
    let video = output.mp4.as_ref().and_then(|mp4| {
        mp4.report
            .streams
            .iter()
            .find_map(|stream| match stream.shape {
                Shape::Video { width, height, .. } => Some((stream.duration, (width, height))),
                Shape::Audio { .. } => None,
            })
    });
    Finished {
        duration: video
            .map(|(duration, _)| duration)
            .or((declared > Duration::ZERO).then_some(declared)),
        bytes: output.bytes,
        resolution: video
            .map(|(_, resolution)| resolution)
            .filter(|&(width, height)| width > 0 && height > 0),
    }
}

fn save_ending(tx: &Transaction<'_>, id: TaskId, ending: &Ending) -> Result<(), Error> {
    store::set_status(tx, id, &ending.status)?;
    if let Some(result) = &ending.result {
        store::set_result(tx, id, result)?;
    }
    if let Some(total) = ending.total {
        store::set_total(tx, id, total)?;
    }
    Ok(())
}

/// 照任务建请求：任务目录照 [`Task::work_dir`] 指定，没有时报 [`Error::NoWorkDir`]；不是直播时不开录制，来源是直播就
/// 报 [`hs_m3u8_core::Error::LiveNotEnabled`]。
fn request(task: &Task) -> Result<JobRequest, Error> {
    let spec = &task.spec;
    let mut source = Source::new(spec.url.clone());
    source.preference = Preference::from(&spec.preference);
    source.http.headers.clone_from(&spec.headers);
    let mut options = spec.output.options();
    options.work_dir = Some(task.work_dir.clone().ok_or(Error::NoWorkDir(task.id))?);
    let mut request = JobRequest::new(source, options);
    if !spec.live {
        request.live = None;
    }
    Ok(request)
}

/// 加锁；持锁期间只做内存与 SQLite 的读写，不会 panic，锁中毒即不变量被破坏。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("持锁期间不会 panic")
}
