//! 任务服务：持有核心的 [`Engine`] 与它的 tokio 运行时，按存储里的任务开始、暂停、继续、停止与放弃，把节流后的
//! 进度与结束状态经事件通道交给界面。

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use hs_m3u8_core::hls::Preference;
use hs_m3u8_core::remux::Shape;
use hs_m3u8_core::{
    CancellationToken, Engine, Job, JobControl, JobRequest, Leftover, Output, Probe, Progress,
    Saved, Source, Stage, Unsettled,
};
use rusqlite::Transaction;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::preference::StoredPreference;
use crate::store::{self, Store};
use crate::{Draft, Error, Finished, Status, Task, TaskId, TaskSpec, naming};

/// 同一任务两次进度事件之间至少隔多久：界面每秒最多重画十次进度。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// 交给界面的事件。
#[derive(Debug)]
pub enum Event {
    /// 运行中任务的进度快照；同一任务两次之间至少隔 100 毫秒，进度没变时不发
    Progress { id: TaskId, progress: Box<Progress> },
    /// 任务停下，已写进存储的状态；状态为 [`Status::Done`] 时 `result` 为记下的结果、`output` 为核心给的完整结果
    Ended {
        id: TaskId,
        status: Status,
        result: Option<Finished>,
        output: Option<Box<Output>>,
    },
    /// 写存储失败。`ended` 为 Some 时任务已按它停下（完成时 `output` 为写出的结果），但状态没能写进存储：服务在内存里
    /// 留着它，下一次写存储（任何操作）或服务关闭时补写，[`Service::tasks`] 也按它返回；补写之前应用异常退出的，下次
    /// 打开时按暂停处理。`ended` 为 None 时任务照常运行，没写进去的是点播的总片数（只影响任务不在运行时显示的进度）
    StoreFailed {
        id: TaskId,
        ended: Option<Status>,
        result: Option<Finished>,
        output: Option<Box<Output>>,
        error: Error,
    },
    /// 新建任务时存储写不进，已开始的任务取消了，但它建出的任务目录没能删干净；目录不在任何任务名下，要用户处理
    Orphaned { work_dir: PathBuf, reason: String },
}

/// 事件的接收端，在任何执行器上都可等待。
pub type Events = mpsc::UnboundedReceiver<Event>;

/// 一个正在运行的任务。
struct Running {
    control: JobControl,
    /// 停下（状态已写进存储或记为待补写、已从运行表里去掉）时变为 true
    finished: watch::Receiver<bool>,
}

/// 任务停下时要写进存储的。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ending {
    status: Status,
    result: Option<Finished>,
}

/// 服务的状态，运行时里的任务与界面共用。
struct Shared {
    engine: Engine,
    store: Mutex<Store>,
    running: Mutex<HashMap<TaskId, Running>>,
    /// 正在放弃的任务：放弃做完（成功或失败）之前不能继续或移除
    discarding: Mutex<HashSet<TaskId>>,
    /// 停下了、但状态没能写进存储的任务：下一次写存储时补写
    unsaved: Mutex<HashMap<TaskId, Ending>>,
    events: mpsc::UnboundedSender<Event>,
}

pub struct Service {
    shared: Arc<Shared>,
    /// 最后释放：释放时等运行时里的任务停下，它们还要用 `shared`
    runtime: Runtime,
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
            running: Mutex::new(HashMap::new()),
            discarding: Mutex::new(HashSet::new()),
            unsaved: Mutex::new(HashMap::new()),
            events,
        });
        Ok((Service { shared, runtime }, receiver))
    }

    /// 全部任务，按新建先后；停下了而状态还没补写进存储的，按停下时的状态。
    pub fn tasks(&self) -> Result<Vec<Task>, Error> {
        let mut tasks = {
            let mut store = lock(&self.shared.store);
            let tx = store.transaction()?;
            store::list(&tx)?
        };
        let unsaved = lock(&self.shared.unsaved);
        for task in &mut tasks {
            if let Some(ending) = unsaved.get(&task.id) {
                task.status = ending.status.clone();
                task.result.clone_from(&ending.result);
            }
        }
        Ok(tasks)
    }

    /// 探测来源；返回的句柄在任何执行器上都可等待，丢弃它不取消探测。
    pub fn probe(&self, source: Source) -> JoinHandle<Result<Probe, hs_m3u8_core::Error>> {
        let engine = self.shared.engine.clone();
        self.runtime
            .spawn(async move { engine.probe(&source).await })
    }

    /// 按 `draft` 新建任务并开始，返回存进存储的任务；`created_at` 为新建时间（Unix 秒）。输出放在 `draft.folder` 里，
    /// 主名由地址得出（见 [`crate::title_of`]），避开磁盘上已有的与存储里各任务占用的输出与任务目录（不分大小写与
    /// Unicode 写法），被占用时加编号；任务的标题即这个主名。`draft.folder` 不是绝对路径、选轨偏好存不了时报
    /// [`Error::InvalidInput`]。取名要查磁盘（网络盘上可能慢），在运行时的阻塞线程上做；返回的句柄在任何执行器上都可
    /// 等待。存储写不进时什么也不留：已开始的任务取消，它建出的任务目录删掉，删不干净时发 [`Event::Orphaned`]。
    pub fn create(&self, draft: Draft, created_at: i64) -> JoinHandle<Result<Task, Error>> {
        let shared = self.shared.clone();
        self.runtime
            .spawn_blocking(move || shared.create(draft, created_at))
    }

    /// 继续暂停或失败的任务：照新建时的请求再开始，任务目录里已完成的分片不重下。
    pub fn resume(&self, id: TaskId) -> Result<(), Error> {
        let _entered = self.runtime.enter();
        self.shared.resume(id)
    }

    /// 任务目录里已完成的内容（见 [`Engine::inspect`]）：任务不在运行时显示已下了多少、继续前算出续传的起点。返回的句柄
    /// 在任何执行器上都可等待。
    pub fn inspect(&self, id: TaskId) -> JoinHandle<Result<Option<Saved>, Error>> {
        let shared = self.shared.clone();
        self.runtime.spawn(async move {
            let task = {
                let mut store = lock(&shared.store);
                let tx = store.transaction()?;
                store::get(&tx, id)?.ok_or(Error::NotFound(id))?
            };
            let work_dir = task.spec.output.options().resolved_work_dir()?;
            Ok(shared.engine.inspect(&work_dir).await?)
        })
    }

    /// 只从存储里删掉已完成的任务，不碰磁盘（任务目录已在完成时删掉，输出归用户）。不是已完成的报
    /// [`Error::NotFinished`]：它们的任务目录要用 [`Service::discard`] 一起删。
    pub fn forget(&self, id: TaskId) -> Result<(), Error> {
        if lock(&self.shared.discarding).contains(&id) {
            return Err(Error::Discarding(id));
        }
        let finished = self
            .tasks()?
            .into_iter()
            .find(|task| task.id == id)
            .ok_or(Error::NotFound(id))?
            .status
            == Status::Done;
        if !finished {
            return Err(Error::NotFinished(id));
        }
        self.shared.write(|tx| store::delete(tx, id))?;
        lock(&self.shared.unsaved).remove(&id);
        Ok(())
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
    /// 做完之前任务记为正在放弃，继续与移除都报 [`Error::Discarding`]，再放弃一次同样报这个错误。返回的句柄在任何执行器
    /// 上都可等待。
    pub fn discard(&self, id: TaskId) -> JoinHandle<Result<Vec<Leftover>, Error>> {
        if !lock(&self.shared.discarding).insert(id) {
            return self
                .runtime
                .spawn(async move { Err(Error::Discarding(id)) });
        }
        let guard = Discarding {
            id,
            shared: self.shared.clone(),
        };
        let finished = lock(&self.shared.running).get(&id).map(|running| {
            running.control.cancel();
            running.finished.clone()
        });
        self.runtime.spawn(async move {
            let shared = guard.shared.clone();
            let result = run_discard(id, &shared, finished).await;
            drop(guard);
            result
        })
    }
}

impl Drop for Service {
    /// 补写还没写进存储的结束状态；补不进时什么也做不了（下次打开时按暂停处理）。
    fn drop(&mut self) {
        // 关闭时没有人能处理错误；补写失败的状态下次打开时按暂停处理
        let _unsaved = self.shared.write(|_| Ok(()));
    }
}

/// 放弃中的登记：放弃做完或中途 panic 时都从放弃中集合里去掉。
struct Discarding {
    id: TaskId,
    shared: Arc<Shared>,
}

impl Drop for Discarding {
    fn drop(&mut self) {
        lock(&self.shared.discarding).remove(&self.id);
    }
}

/// [`Service::discard`] 的步骤：等任务停下、删任务目录、删存储里的记录。
async fn run_discard(
    id: TaskId,
    shared: &Shared,
    finished: Option<watch::Receiver<bool>>,
) -> Result<Vec<Leftover>, Error> {
    if let Some(mut finished) = finished {
        // 运行登记释放时先置为 true 再丢弃发送端，等到值或发送端已丢弃都说明任务已停下
        let _stopped = finished.wait_for(|done| *done).await;
    }
    let task = {
        let mut store = lock(&shared.store);
        let tx = store.transaction()?;
        store::get(&tx, id)?.ok_or(Error::NotFound(id))?
    };
    let work_dir = task.spec.output.options().resolved_work_dir()?;
    let leftovers = shared
        .engine
        .discard(&work_dir, Unsettled::Fail, &CancellationToken::new())
        .await?;
    shared.write(|tx| store::delete(tx, id))?;
    lock(&shared.unsaved).remove(&id);
    Ok(leftovers)
}

/// 运行中任务的登记：任务停下（或收尾的任务 panic）时从运行表里去掉，并告诉等它停下的放弃。
struct Registered {
    id: TaskId,
    shared: Arc<Shared>,
    finished: watch::Sender<bool>,
}

impl Drop for Registered {
    fn drop(&mut self) {
        lock(&self.shared.running).remove(&self.id);
        self.finished.send_replace(true);
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
        drop(store);
        self.clear_unsaved(applied);
        Ok(value)
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

    /// 已提交的补写从待补写里去掉；其间同一任务又记了新的结束状态时留着新的。
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
        let mut store = lock(&self.store);
        let tx = store.transaction()?;
        let applied = self.apply_unsaved(&tx)?;
        let (title, output) = naming::output_for(
            &draft.folder,
            &naming::title_of(&draft.url),
            draft.output,
            &store::taken_paths(&tx)?,
        )?;
        let spec = TaskSpec {
            title,
            url: draft.url,
            preference,
            headers: draft.headers,
            output,
            live: draft.live,
        };
        let id = store::insert(&tx, &spec, created_at)?;
        let task = store::get(&tx, id)?.expect("同一事务里刚插入的行读得到");
        let job = self.launch(id, request(&spec))?;
        if let Err(error) = tx.commit() {
            drop(store);
            let work_dir = spec.output.options().resolved_work_dir()?;
            self.abandon(job, Some(work_dir));
            return Err(Error::Store(error));
        }
        drop(store);
        self.clear_unsaved(applied);
        self.track(id, job, task.total);
        Ok(task)
    }

    fn resume(self: &Arc<Self>, id: TaskId) -> Result<(), Error> {
        if lock(&self.running).contains_key(&id) {
            return Err(Error::Running(id));
        }
        if lock(&self.discarding).contains(&id) {
            return Err(Error::Discarding(id));
        }
        let mut store = lock(&self.store);
        let tx = store.transaction()?;
        let applied = self.apply_unsaved(&tx)?;
        let task = store::get(&tx, id)?.ok_or(Error::NotFound(id))?;
        store::set_status(&tx, id, &Status::Running)?;
        let job = self.launch(id, request(&task.spec))?;
        if let Err(error) = tx.commit() {
            drop(store);
            // 任务目录是这个任务原有的，只取消不删
            self.abandon(job, None);
            return Err(Error::Store(error));
        }
        drop(store);
        self.clear_unsaved(applied);
        self.track(id, job, task.total);
        Ok(())
    }

    /// 开始任务；`start` 的参数错误照原样报出。调用方须在运行时里（[`Engine::start`] 的要求）。
    fn launch(&self, id: TaskId, request: JobRequest) -> Result<Job, Error> {
        if lock(&self.running).contains_key(&id) {
            return Err(Error::Running(id));
        }
        Ok(self.engine.start(request)?)
    }

    /// 存储写不进而不要的任务：取消，等它停下；`work_dir` 为 Some 时（新建的任务）再删掉它建出的任务目录，删不干净时
    /// 发 [`Event::Orphaned`]。
    fn abandon(self: &Arc<Self>, job: Job, work_dir: Option<PathBuf>) {
        job.control().cancel();
        let shared = self.clone();
        tokio::spawn(async move {
            // 已取消，结果只能是取消或取消前就出的错，两者都不再需要
            let _cancelled = job.wait().await;
            let Some(work_dir) = work_dir else {
                return;
            };
            let reason = match shared
                .engine
                .discard(&work_dir, Unsettled::Fail, &CancellationToken::new())
                .await
            {
                Ok(leftovers) if leftovers.is_empty() => return,
                Ok(leftovers) => leftovers
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("；"),
                Err(error) => error.to_string(),
            };
            // 接收端随界面关闭而丢弃时没有谁能处理
            let _closed = shared.events.send(Event::Orphaned { work_dir, reason });
        });
    }

    /// 登记运行中的任务，转发它的进度，等它停下后把状态写进存储并发出 [`Event::Ended`]。`total` 为存储里记着的点播
    /// 总片数；点播下载开始后的总片数与它不同时记下。
    fn track(self: &Arc<Self>, id: TaskId, job: Job, total: Option<usize>) {
        let control = job.control();
        let (finished_tx, finished) = watch::channel(false);
        lock(&self.running).insert(
            id,
            Running {
                control: control.clone(),
                finished,
            },
        );
        let registered = Registered {
            id,
            shared: self.clone(),
            finished: finished_tx,
        };
        let shared = self.clone();
        let mut progress = control.progress();
        let forwarding = tokio::spawn(async move {
            let mut total = total;
            while progress.changed().await.is_ok() {
                let snapshot = progress.borrow_and_update().clone();
                // 点播的总片数在下载开始时就定了；直播与准备阶段不记
                let downloading = snapshot.stage == Stage::Downloading;
                if downloading
                    && snapshot.segments.total > 0
                    && total != Some(snapshot.segments.total)
                {
                    total = Some(snapshot.segments.total);
                    if let Err(error) =
                        shared.write(|tx| store::set_total(tx, id, snapshot.segments.total))
                    {
                        let failed = Event::StoreFailed {
                            id,
                            ended: None,
                            result: None,
                            output: None,
                            error,
                        };
                        if shared.events.send(failed).is_err() {
                            break;
                        }
                    }
                }
                if shared
                    .events
                    .send(Event::Progress {
                        id,
                        progress: Box::new(snapshot),
                    })
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(PROGRESS_INTERVAL).await;
            }
        });
        let shared = self.clone();
        tokio::spawn(async move {
            let result = job.wait().await;
            forwarding.abort();
            let (ending, output) = match result {
                Ok(output) => {
                    let declared = control.progress().borrow().duration;
                    let ending = Ending {
                        status: Status::Done,
                        result: Some(finished_result(&output, declared)),
                    };
                    (ending, Some(Box::new(output)))
                }
                Err(hs_m3u8_core::Error::Cancelled) => (
                    Ending {
                        status: Status::Paused,
                        result: None,
                    },
                    None,
                ),
                Err(error) => (
                    Ending {
                        status: Status::Failed(error.to_string()),
                        result: None,
                    },
                    None,
                ),
            };
            let saved = shared.write(|tx| save_ending(tx, id, &ending));
            if saved.is_err() {
                lock(&shared.unsaved).insert(id, ending.clone());
            }
            drop(registered);
            let Ending { status, result } = ending;
            let event = match saved {
                Ok(()) => Event::Ended {
                    id,
                    status,
                    result,
                    output,
                },
                Err(error) => Event::StoreFailed {
                    id,
                    ended: Some(status),
                    result,
                    output,
                    error,
                },
            };
            // 接收端随界面关闭而丢弃时没有谁要这个事件；状态已写进存储或留着待补写
            let _closed = shared.events.send(event);
        });
    }
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
    Ok(())
}

/// 照任务的内容建请求：不是直播时不开录制，来源是直播就报 [`hs_m3u8_core::Error::LiveNotEnabled`]。
fn request(spec: &TaskSpec) -> JobRequest {
    let mut source = Source::new(spec.url.clone());
    source.preference = Preference::from(&spec.preference);
    source.http.headers.clone_from(&spec.headers);
    let mut request = JobRequest::new(source, spec.output.options());
    if !spec.live {
        request.live = None;
    }
    request
}

/// 加锁；持锁期间只做内存与 SQLite 的读写，不会 panic，锁中毒即不变量被破坏。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("持锁期间不会 panic")
}
