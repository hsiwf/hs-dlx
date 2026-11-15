//! BT 任务的引擎：整个进程一个 libtorrent 会话，第一次用到时才建（不用 BT 时不开监听端口、不连 DHT）。会话里只有
//! 运行中的 BT 任务与正在取种子信息的磁力链接，每个种子归一个异步任务管：一条线程取会话的事件，按种子的编号转给
//! 管它的那个（[`Owner`]）。
//!
//! 种子的一切（种子信息、tracker、根名、选了的文件、已有的块）都在它的续传数据里，存进任务存储；停下时先要续传数据、
//! 紧接着把缓存写到磁盘，两者都回来、其间没有写盘出错，存下的已有块才不多于磁盘上的（标为可信）。

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle as ThreadHandle;
use std::time::Duration;

use hs_dlx_torrent::{
    AddOptions, Alert, AlertKind, Removal, Session, SessionConfig, State, TorrentId,
};
use tokio::sync::{Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::{Error, Failure, FailureKind, Seeding};

/// 取事件时最多等多久（毫秒）：关闭时取事件的线程最晚这么久后发现要退出。
const PUMP_WAIT_MS: u32 = 250;
/// 运行中多久查一次种子的状态、发一次进度。
const STATUS_INTERVAL: Duration = Duration::from_millis(500);
/// 等 libtorrent 一步的结果（续传数据、写完缓存、移除完、删完文件）最多等多久：都只是本机的操作。
const STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// 连着节点而凑不齐全部内容要连续这么多次状态（每次 [`STATUS_INTERVAL`]，共 3 秒）才算资源不足：刚连上的节点在报来
/// 它有哪些块之前也计入节点数，开始时会短暂凑不齐。
const SCARCE_TICKS: u32 = 6;
/// 操作系统在用户打开过的目录里自己建的文件（访达的显示设置、Windows 资源管理器的缩略图与目录设置）：放弃 BT 任务时
/// 随下了的文件一起删，不算用户放进去的东西。
const OS_LITTER: [&str; 3] = [".DS_Store", "Thumbs.db", "desktop.ini"];

/// BT 会话连哪里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorrentNetwork {
    /// 监听本机的全部地址（端口沿用上次的，第一次由系统挑），用 DHT、UPnP、NAT-PMP 与本地发现
    Internet,
    /// 只监听回环地址、不找外面的节点：用于测试
    Loopback,
}

/// 种子里的一个文件（不含对齐用的填充文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentFile {
    /// 在种子里的序号，选文件时用它
    pub index: i32,
    /// 相对保存目录的路径（多文件的以根名的目录开头），本平台的分隔符；不是 UTF-8 的字节换成了替换字符，只用于显示
    pub path: String,
    /// 字节数
    pub size: u64,
    /// 要下载
    pub wanted: bool,
}

/// 续传数据里的种子信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentInfo {
    /// 种子的编号：信息哈希的十六进制（有 v1 的为它，只有 v2 时为截到 20 字节的 v2 的）
    pub info_hash: String,
    /// 种子名：单文件的为文件名，多文件的为顶层目录名；不是 UTF-8 的字节换成了替换字符
    pub name: String,
    /// 私有种子：只经种子里的 tracker 找节点
    pub private: bool,
    pub files: Vec<TorrentFile>,
    /// 续传数据里记的已有的块落在要下的文件里的字节数
    pub done: u64,
}

impl TorrentInfo {
    /// 读出续传数据里的种子信息；读不出（不是续传数据、没有种子信息）时报 [`Error::InvalidInput`]。
    pub fn of(resume: &[u8]) -> Result<TorrentInfo, Error> {
        let metadata = hs_dlx_torrent::describe(resume)
            .map_err(|error| Error::InvalidInput(format!("读不出种子信息：{error}")))?;
        Ok(TorrentInfo {
            info_hash: metadata.id.as_str().to_owned(),
            name: metadata.name,
            private: metadata.private,
            files: metadata
                .files
                .into_iter()
                .map(|file| TorrentFile {
                    index: file.index,
                    path: file.path,
                    size: u64::try_from(file.size).expect("libtorrent 的文件大小不为负"),
                    wanted: file.wanted,
                })
                .collect(),
            done: metadata.done,
        })
    }

    /// 要下的字节数。
    pub fn wanted(&self) -> u64 {
        self.files
            .iter()
            .filter(|file| file.wanted)
            .map(|file| file.size)
            .sum()
    }

    /// 单文件的种子（内容的根是文件，不是目录）。
    pub fn single_file(&self) -> bool {
        matches!(self.files.as_slice(), [only] if only.path == self.name)
    }
}

/// BT 任务存着的续传数据里的种子信息（[`crate::Service::inspect_torrent`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTorrent {
    pub info: TorrentInfo,
    /// 已有块的记录可信；否时（上次没正常停下）`info.done` 不作数，下次继续时整体校验磁盘上的文件
    pub trusted: bool,
}

/// 新建任务前定下内容放哪、下哪些（见 [`hs_dlx_torrent::place`]）；定不下（根名不是一个路径段、序号不是种子里的文件、
/// 读不出种子信息）时报 [`Error::InvalidInput`]。
pub(crate) fn place(resume: &[u8], root: &str, wanted: &[i32]) -> Result<Vec<u8>, Error> {
    hs_dlx_torrent::place(resume, root, wanted)
        .map_err(|error| Error::InvalidInput(format!("定不下种子的根名与要下的文件：{error}")))
}

/// 种子文件（bencode 写的内容）转成续传数据，供新建任务（[`crate::DraftKind::Torrent`]）；不是种子文件时报
/// [`Error::InvalidInput`]。
pub fn torrent_from_file(torrent: &[u8]) -> Result<Vec<u8>, Error> {
    hs_dlx_torrent::resume_from_torrent_file(torrent)
        .map_err(|error| Error::InvalidInput(format!("不是种子文件：{error}")))
}

/// 磁力链接取种子信息的进度。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataProgress {
    /// 连着的节点数
    pub peers: u32,
    /// 最近一次 tracker 报的错（libtorrent 的说明，英文，含 tracker 的地址）；没报过时为 None
    pub tracker_error: Option<String>,
}

/// 运行中的 BT 任务在做什么。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TorrentPhase {
    /// 校验磁盘上已有的文件；`progress` 为校验了的比例，0 到 1
    Checking {
        progress: f32,
    },
    Downloading,
    /// 选了的文件都下完了，正在存续传数据、转去做种或从会话里移除
    Finishing,
    /// 下完了，在做种
    Seeding,
}

/// 运行中的 BT 任务的进度快照。
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentProgress {
    pub phase: TorrentPhase,
    /// 要下的字节数与其中已下完并校验过的
    pub wanted: u64,
    pub done: u64,
    /// 这次运行收到的内容字节数（含校验没过、丢掉的）：算速度用
    pub received: u64,
    /// 累计上传的内容字节数与做种的秒数，含之前各次运行的
    pub uploaded: u64,
    pub seeding_seconds: u64,
    /// 下载与上传的速度，字节每秒，只算内容
    pub download_rate: u64,
    pub upload_rate: u64,
    /// 连着的节点数，其中的做种者
    pub peers: u32,
    pub seeds: u32,
    /// 要下的块数，其中能凑齐的（自己已有，或在线的节点里至少有一个有）：少于 `pieces` 时眼下的节点凑不齐全部内容
    pub pieces: u32,
    pub reachable: u32,
    /// 资源不足：下载中连着节点，凑不齐全部内容已持续 3 秒以上
    pub scarce: bool,
    /// 这次运行里最近一次 tracker 报的错（libtorrent 的说明，英文，含 tracker 的地址）；没报过时为 None。私有种子只经
    /// tracker 找节点，它报错时多半一直连不上节点
    pub tracker_error: Option<String>,
}

impl TorrentProgress {
    fn of(status: &hs_dlx_torrent::Status) -> TorrentProgress {
        let count = |value: i32| u32::try_from(value).expect("libtorrent 的计数不为负");
        let bytes = |value: i64| u64::try_from(value).expect("libtorrent 的字节数不为负");
        let phase = match status.state() {
            Some(State::CheckingFiles | State::CheckingResumeData) => TorrentPhase::Checking {
                progress: status.progress_ppm as f32 / 1e6,
            },
            Some(State::Finished | State::Seeding) => TorrentPhase::Finishing,
            Some(State::Downloading | State::DownloadingMetadata) | None => {
                TorrentPhase::Downloading
            }
        };
        TorrentProgress {
            phase,
            wanted: bytes(status.total_wanted),
            done: bytes(status.total_wanted_done),
            received: bytes(status.downloaded),
            uploaded: bytes(status.all_time_upload),
            seeding_seconds: bytes(status.seeding_seconds),
            download_rate: bytes(status.download_rate.into()),
            upload_rate: bytes(status.upload_rate.into()),
            peers: count(status.peers),
            seeds: count(status.seeds),
            pieces: count(status.pieces),
            reachable: count(status.reachable_pieces),
            scarce: false,
            tracker_error: None,
        }
    }
}

/// 存进任务存储的续传数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resume {
    pub data: Vec<u8>,
    /// 记的已有块不多于磁盘上的：否时下次加入不信这些记录、整体校验磁盘上的文件
    pub trusted: bool,
}

/// 转给管种子的异步任务的。
enum Routed {
    Alert(Alert),
    /// 事件队列满了丢了一些，可能有这个种子的：在等的那一步可能再也等不到
    Lost,
}

/// 有会话之后的引擎。
pub(crate) struct Engine {
    session: Session,
    /// 种子的编号 → 管它的异步任务
    routes: Mutex<HashMap<TorrentId, mpsc::UnboundedSender<Routed>>>,
    /// 每有一个种子移除了（或删完文件）就通知：等 [`Session::removing`] 变为否的在等它
    removed: Notify,
    /// 关闭了：取事件的线程退出
    stop: AtomicBool,
}

impl Engine {
    fn pump(&self) {
        while !self.stop.load(Ordering::Relaxed) {
            let alerts = self
                .session
                .pop_alerts(PUMP_WAIT_MS)
                .unwrap_or_else(|error| {
                    panic!("BT 会话的事件取不出，会话里的状态不再可信：{error}")
                });
            for alert in alerts {
                self.dispatch(alert);
            }
        }
    }

    fn dispatch(&self, alert: Alert) {
        match alert.kind {
            AlertKind::AlertsDropped => {
                for route in lock(&self.routes).values() {
                    // 接收端随管种子的异步任务结束而丢弃：那个种子已没人在等
                    let _gone = route.send(Routed::Lost);
                }
            }
            // 简化：监听与端口映射的结果、加入失败（同步的加入已报错）都不用，诊断时再报给界面
            AlertKind::ListenSucceeded
            | AlertKind::ListenFailed
            | AlertKind::PortMapped
            | AlertKind::PortMapFailed
            | AlertKind::AddFailed => {}
            kind => {
                let settled = matches!(
                    kind,
                    AlertKind::Removed | AlertKind::Deleted | AlertKind::DeleteFailed
                );
                if let Ok(id) = TorrentId::parse(&alert.torrent)
                    && let Some(route) = lock(&self.routes).get(&id)
                {
                    // 同上
                    let _gone = route.send(Routed::Alert(alert));
                }
                if settled {
                    self.removed.notify_waiters();
                }
            }
        }
    }

    /// 由管种子 `id` 的异步任务接手它的事件；已有别的在管时报错（同一个种子正在取种子信息或下载）。
    fn own(self: &Arc<Self>, id: &TorrentId) -> Result<Owner, Error> {
        let (sender, alerts) = mpsc::unbounded_channel();
        let mut routes = lock(&self.routes);
        if routes.contains_key(id) {
            return Err(Error::Torrent(format!(
                "种子 {id} 正在别处使用（取种子信息或下载）"
            )));
        }
        routes.insert(id.clone(), sender);
        Ok(Owner {
            engine: self.clone(),
            id: id.clone(),
            alerts,
        })
    }

    /// 等种子 `id` 移除完（之前移除的还没完时再加入会报错）；[`STEP_TIMEOUT`] 内没完报 [`Error::Torrent`]。
    async fn settled(&self, id: &TorrentId) -> Result<(), Error> {
        let waiting = async {
            loop {
                // 先登记再查：查完到等待之间来的通知不会漏掉
                let notified = self.removed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if !self.session.removing(id) {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(STEP_TIMEOUT, waiting)
            .await
            .map_err(|_| Error::Torrent(format!("种子 {id} 之前的移除 10 秒内没做完")))
    }
}

/// 管一个种子的异步任务手里的：它的事件；释放时不再接收。
struct Owner {
    engine: Arc<Engine>,
    id: TorrentId,
    alerts: mpsc::UnboundedReceiver<Routed>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        lock(&self.engine.routes).remove(&self.id);
    }
}

impl Owner {
    fn session(&self) -> &Session {
        &self.engine.session
    }

    fn libtorrent(&self, what: &str) -> impl FnOnce(hs_dlx_torrent::Error) -> Error + use<> {
        let what = format!("{what}（种子 {}）", self.id);
        move |error| Error::Torrent(format!("{what}：{error}"))
    }

    /// 下一条事件；事件丢了时为 [`Routed::Lost`]。转发的一端只在引擎释放时丢弃，而引擎在这里还被持有。
    async fn next(&mut self) -> Routed {
        self.alerts
            .recv()
            .await
            .expect("转发事件的一端随引擎释放，这里还持有引擎")
    }

    /// 等到 `wanted` 认的那条事件并返回它，[`STEP_TIMEOUT`] 内没等到、或事件丢了时报 [`Error::Torrent`]；其间的别的事件
    /// 交给 `others`。
    async fn step(
        &mut self,
        what: &str,
        mut wanted: impl FnMut(&Alert) -> bool,
        mut others: impl FnMut(&Alert),
    ) -> Result<Alert, Error> {
        let id = self.id.clone();
        let waiting = async {
            loop {
                match self.next().await {
                    Routed::Alert(alert) if wanted(&alert) => return Ok(alert),
                    Routed::Alert(alert) => others(&alert),
                    Routed::Lost => {
                        return Err(Error::Torrent(format!(
                            "等{what}时 BT 会话的事件丢了（种子 {id}）"
                        )));
                    }
                }
            }
        };
        tokio::time::timeout(STEP_TIMEOUT, waiting)
            .await
            .map_err(|_| Error::Torrent(format!("10 秒内没等到{what}（种子 {}）", self.id)))?
    }

    /// 从会话里移除（照 `removal` 连带删文件）并等它移除完；删文件失败时报 [`Error::Torrent`]，说明为 libtorrent 的。
    async fn remove(&mut self, removal: Removal) -> Result<(), Error> {
        self.session()
            .remove(&self.id, removal)
            .map_err(self.libtorrent("移除"))?;
        let (mut removed, mut deleted) = (false, removal == Removal::Keep);
        let mut failed = None;
        while !(removed && deleted) {
            let alert = self
                .step(
                    "移除完",
                    |alert| {
                        matches!(
                            alert.kind,
                            AlertKind::Removed | AlertKind::Deleted | AlertKind::DeleteFailed
                        )
                    },
                    |_| {},
                )
                .await?;
            match alert.kind {
                AlertKind::Removed => removed = true,
                AlertKind::Deleted => deleted = true,
                _ => {
                    deleted = true;
                    failed = Some(alert.message);
                }
            }
        }
        match failed {
            Some(message) => Err(Error::Torrent(format!("删文件没删干净：{message}"))),
            None => Ok(()),
        }
    }
}

/// 引擎的持有者：第一次用到时建会话与取事件的线程，关闭时收掉。
pub(crate) struct Bt {
    network: TorrentNetwork,
    /// 关闭了（[`Bt::close`]）：不再建会话
    closed: AtomicBool,
    /// 磁力链接取种子信息时种子的保存目录：只取信息、不下内容，libtorrent 也可能在里面建空文件，不放进用户的目录
    probe_dir: PathBuf,
    running: Mutex<Option<Running>>,
}

struct Running {
    engine: Arc<Engine>,
    pump: ThreadHandle<()>,
}

/// 关闭时要存下的会话状态。
pub(crate) struct SessionState {
    /// 监听的端口；还没监听成功时为 None
    pub port: Option<u16>,
    /// DHT 的状态（路由表里的节点）
    pub dht: Vec<u8>,
}

impl Bt {
    pub fn new(network: TorrentNetwork) -> Bt {
        Bt {
            network,
            closed: AtomicBool::new(false),
            probe_dir: std::env::temp_dir().join("hs-dlx-magnet"),
            running: Mutex::new(None),
        }
    }

    /// 引擎；还没有时照上次存下的 `saved`（端口与 DHT 状态）建会话。已关闭时报 [`Error::Closed`]，会话建不出时报
    /// [`Error::Torrent`]。
    pub fn engine(
        &self,
        saved: SessionState,
        upload_limit: Option<NonZeroU32>,
    ) -> Result<Arc<Engine>, Error> {
        let mut running = lock(&self.running);
        if self.closed.load(Ordering::Relaxed) {
            return Err(Error::Closed);
        }
        if let Some(running) = &*running {
            return Ok(running.engine.clone());
        }
        let version = env!("CARGO_PKG_VERSION");
        let (listen, discovery) = match self.network {
            TorrentNetwork::Internet => {
                let port = saved.port.unwrap_or(0);
                (format!("0.0.0.0:{port},[::]:{port}"), true)
            }
            TorrentNetwork::Loopback => ("127.0.0.1:0".to_owned(), false),
        };
        let session = Session::new(&SessionConfig {
            listen_interfaces: listen,
            dht: discovery,
            upnp: discovery,
            natpmp: discovery,
            lsd: discovery,
            user_agent: format!("hs-dlx/{version}"),
            peer_fingerprint: fingerprint(version),
            upload_limit: limit_of(upload_limit),
            state: saved.dht,
        })
        .map_err(|error| Error::Torrent(format!("BT 会话建不出：{error}")))?;
        let engine = Arc::new(Engine {
            session,
            routes: Mutex::new(HashMap::new()),
            removed: Notify::new(),
            stop: AtomicBool::new(false),
        });
        let pumped = engine.clone();
        let pump = std::thread::Builder::new()
            .name("hs-dlx-bt-alerts".into())
            .spawn(move || pumped.pump())
            .map_err(|cause| Error::Io {
                action: "启动",
                path: "BT 事件线程".into(),
                cause,
            })?;
        *running = Some(Running {
            engine: engine.clone(),
            pump,
        });
        Ok(engine)
    }

    /// 关闭：停下取事件的线程（最多等 [`PUMP_WAIT_MS`]），返回要存下的会话状态；没建过会话时为 None。会话随最后一个
    /// 持有引擎的释放而关，释放时阻塞（见 [`Session`]）。
    pub fn close(&self) -> Result<Option<SessionState>, Error> {
        let taken = {
            let mut running = lock(&self.running);
            self.closed.store(true, Ordering::Relaxed);
            running.take()
        };
        let Some(running) = taken else {
            return Ok(None);
        };
        running.engine.stop.store(true, Ordering::Relaxed);
        if let Err(panic) = running.pump.join() {
            std::panic::resume_unwind(panic);
        }
        let session = &running.engine.session;
        let dht = session
            .session_state()
            .map_err(|error| Error::Torrent(format!("BT 会话的 DHT 状态写不出：{error}")))?;
        let port = Some(session.listen_port()).filter(|&port| port != 0);
        Ok(Some(SessionState { port, dht }))
    }

    /// 改上传限速（字节每秒，None 为不限）；还没建会话时什么也不做（建时照设置）。
    pub fn set_upload_limit(&self, upload_limit: Option<NonZeroU32>) {
        if let Some(running) = &*lock(&self.running) {
            running
                .engine
                .session
                .set_upload_limit(limit_of(upload_limit));
        }
    }

    pub fn probe_dir(&self) -> &Path {
        &self.probe_dir
    }
}

/// 上传限速在 libtorrent 里的写法：字节每秒，0 为不限；超过 i32 的当作不限。
fn limit_of(limit: Option<NonZeroU32>) -> i32 {
    limit.map_or(0, |limit| i32::try_from(limit.get()).unwrap_or(0))
}

/// 节点 ID 的前缀（Azureus 的写法）：`-HD` + 版本号的主、次、修订各一位 + `0-`，超过 9 的写 9。
fn fingerprint(version: &str) -> String {
    let digit = |part: Option<&str>| {
        part.and_then(|part| part.parse::<u32>().ok())
            .map_or('0', |n| char::from_digit(n.min(9), 10).expect("0 到 9"))
    };
    let mut parts = version.split('.');
    let (major, minor, patch) = (
        digit(parts.next()),
        digit(parts.next()),
        digit(parts.next()),
    );
    format!("-HD{major}{minor}{patch}0-")
}

/// 磁力链接 `uri` 取到种子信息，返回它的续传数据；其间把连着的节点数报进 `progress`。不是磁力链接、或这个种子正在
/// 下载或取种子信息时报 [`Error::Torrent`]。任务被取消（句柄释放）时种子随之从会话里移除。
pub(crate) async fn fetch_metadata(
    engine: Arc<Engine>,
    uri: String,
    probe_dir: PathBuf,
    progress: watch::Sender<MetadataProgress>,
) -> Result<Vec<u8>, Error> {
    let save_path = utf8(&probe_dir)?;
    let id = engine
        .session
        .add_magnet(&uri, save_path)
        .map_err(|error| Error::Torrent(format!("加不进磁力链接：{error}")))?;
    let probe = Probe {
        engine: engine.clone(),
        id: id.clone(),
    };
    // 加进去之后才登记：同一个种子已在会话里时上面就报错了，不会抢别人的事件
    let mut owner = engine.own(&id)?;
    let mut ticks = tokio::time::interval(STATUS_INTERVAL);
    let mut tracker_error = None;
    // 登记之前已取到的，事件不会再来：每次先看状态
    loop {
        let status = owner
            .session()
            .status(&id)
            .map_err(owner.libtorrent("查状态"))?;
        if status.has_metadata {
            break;
        }
        progress.send_replace(MetadataProgress {
            peers: u32::try_from(status.peers).expect("libtorrent 的计数不为负"),
            tracker_error: tracker_error.clone(),
        });
        // 取到种子信息的事件丢了也不要紧：每次都看状态
        tokio::select! {
            routed = owner.next() => {
                if let Routed::Alert(alert) = routed
                    && alert.kind == AlertKind::TrackerError
                {
                    tracker_error = Some(alert.message);
                }
            }
            _ = ticks.tick() => {}
        }
    }
    owner
        .session()
        .save_resume_data(&id)
        .map_err(owner.libtorrent("要续传数据"))?;
    let alert = owner
        .step(
            "续传数据",
            |alert| {
                matches!(
                    alert.kind,
                    AlertKind::ResumeData | AlertKind::ResumeDataFailed
                )
            },
            |_| {},
        )
        .await?;
    if alert.kind == AlertKind::ResumeDataFailed {
        return Err(Error::Torrent(format!(
            "种子信息取到了，续传数据写不出：{}",
            alert.message
        )));
    }
    drop(probe);
    drop(owner);
    Ok(alert.data)
}

/// 取种子信息用的种子：释放（取到、出错或被取消）时从会话里移除，不等移除完（再加入同一个种子的会等）。
struct Probe {
    engine: Arc<Engine>,
    id: TorrentId,
}

impl Drop for Probe {
    fn drop(&mut self) {
        // 移除不了只在它已不在会话里时：那正是要的结果
        let _absent = self.engine.session.remove(&self.id, Removal::Keep);
    }
}

/// 一次 BT 任务的运行怎么结束的。
#[derive(Debug)]
pub(crate) enum Outcome {
    /// 选了的文件都下完了；值为要下的字节数
    Finished(u64),
    Paused,
    Failed(Failure),
}

/// 一次运行的结果：怎么结束的，与停下时要存的续传数据（None 时存储里原有的照旧：没要到，或运行没开始）；下完了照设置
/// 接着做种时为还在会话里的种子（[`seed`]）。
pub(crate) struct Ended {
    pub outcome: Outcome,
    pub resume: Option<Resume>,
    pub seeder: Option<Seeder>,
}

/// 下完了、要接着做种的种子：还在会话里，由它的异步任务接着管。
pub(crate) struct Seeder {
    owner: Owner,
}

/// 运行一个 BT 任务：由续传数据 `resume`（`trusted` 见 [`Resume::trusted`]）加入种子 `info_hash`，下到 `save_path`，进度
/// 交给 `each`，直到下完、出错或 `cancel` 取消（暂停）。停下时要到续传数据；下完了而做种的设置 `seeding` 还没到停的
/// 条件时留在会话里接着做种（[`Ended::seeder`]），否则从会话里移除。
pub(crate) async fn run(
    engine: Arc<Engine>,
    info_hash: &str,
    resume: Resume,
    save_path: PathBuf,
    seeding: &watch::Receiver<Seeding>,
    cancel: CancellationToken,
    mut each: impl FnMut(TorrentProgress),
) -> Ended {
    let failed = |error: Error| Ended {
        outcome: Outcome::Failed(Failure {
            kind: FailureKind::Other,
            message: error.to_string(),
        }),
        resume: None,
        seeder: None,
    };
    let id = match TorrentId::parse(info_hash) {
        Ok(id) => id,
        // 存储里读出的编号认不出：再试也一样
        Err(error) => {
            return Ended {
                outcome: Outcome::Failed(Failure {
                    kind: FailureKind::Unrecoverable,
                    message: Error::Corrupt(format!("种子的编号：{error}")).to_string(),
                }),
                resume: None,
                seeder: None,
            };
        }
    };
    let mut owner = match engine.own(&id) {
        Ok(owner) => owner,
        Err(error) => return failed(error),
    };
    if let Err(error) = engine.settled(&id).await {
        return failed(error);
    }
    let options = AddOptions {
        trust_pieces: resume.trusted,
        paused: false,
        upload_only: false,
    };
    let added = utf8(&save_path).and_then(|path| {
        owner
            .session()
            .add_resume(&resume.data, &id, path, options)
            .map_err(owner.libtorrent("加入"))
    });
    if let Err(error) = added {
        return failed(error);
    }
    let outcome = drive(&mut owner, &cancel, &mut each).await;
    if let Outcome::Finished(wanted) = outcome
        && !reached(&owner, *seeding.borrow(), wanted)
    {
        // 接着做种：存下进度，种子留在会话里
        let (resume, problem) = snapshot(&mut owner).await;
        let Some(failure) = problem else {
            return Ended {
                outcome: Outcome::Finished(wanted),
                resume,
                seeder: Some(Seeder { owner }),
            };
        };
        // 写盘出错：已下的不一定都在磁盘上，不做种，照下面停下时出错处理（移除时出的错不盖过它）
        let _removed = owner.remove(Removal::Keep).await;
        return Ended {
            outcome: Outcome::Failed(failure),
            resume,
            seeder: None,
        };
    }
    // 下完了不做种：存不要的文件的部分块的 .parts 不再有用，一起删
    let removal = if matches!(outcome, Outcome::Finished(_)) {
        Removal::PartFile
    } else {
        Removal::Keep
    };
    let (resume, stopped) = stop(&mut owner, removal).await;
    // 停下时写盘出错或等不到结果：已下的不一定都在磁盘上、之后不一定立即加得进，下完与暂停都改报这个失败
    let outcome = match (outcome, stopped) {
        (Outcome::Failed(failure), _) => Outcome::Failed(failure),
        (_, Err(failure)) => Outcome::Failed(failure),
        (outcome, Ok(())) => outcome,
    };
    Ended {
        outcome,
        resume,
        seeder: None,
    }
}

/// 下完了、选了的文件共 `wanted` 字节的种子到了做种的设置 `seeding` 停的条件没有（按 libtorrent 记的累计上传与做种
/// 时长）；查不到状态时算到了（不接着做种）。
fn reached(owner: &Owner, seeding: Seeding, wanted: u64) -> bool {
    match owner.session().status(&owner.id) {
        Ok(status) => seeding.reached(
            u64::try_from(status.all_time_upload).expect("字节数不为负"),
            wanted,
            u64::try_from(status.seeding_seconds).expect("秒数不为负"),
        ),
        // 不接着做种，移除时照常报出问题
        Err(_) => true,
    }
}

/// 做种为什么停了。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedEnd {
    /// 到了做种设置里停的条件（或设置改成了下完不做种）
    Reached,
    /// 手动停止，或应用退出
    Stopped,
    /// 磁盘上的文件不全（挪走、删了或改了内容）：不补下，不再做种
    Incomplete,
    /// 出错（读文件出错、BT 会话出错等）：说明
    Failed(String),
}

/// 一次做种的结果：为什么停了，与停下时要存的续传数据（None 时存储里原有的照旧）。
pub(crate) struct Seeded {
    pub end: SeedEnd,
    pub resume: Option<Resume>,
}

/// 由续传数据 `resume` 加入已下完的种子 `info_hash`（在 `save_path` 下）去做种：只上传，缺了的块不补下。没有这个种子
/// 的编号、已有别的在管它、之前的移除没做完或加不进时报错。
pub(crate) async fn start_seeding(
    engine: Arc<Engine>,
    info_hash: &str,
    resume: &Resume,
    save_path: &Path,
) -> Result<Seeder, Error> {
    let id = TorrentId::parse(info_hash)
        .map_err(|error| Error::Corrupt(format!("种子的编号：{error}")))?;
    let owner = engine.own(&id)?;
    engine.settled(&id).await?;
    let options = AddOptions {
        trust_pieces: resume.trusted,
        paused: false,
        upload_only: true,
    };
    owner
        .session()
        .add_resume(&resume.data, &id, utf8(save_path)?, options)
        .map_err(owner.libtorrent("加入"))?;
    Ok(Seeder { owner })
}

/// 做种，进度（阶段为校验或 [`TorrentPhase::Seeding`]）交给 `each`，直到到了做种的设置 `seeding`（随时可改）停的条件、
/// `cancel` 取消、校验出文件不全或出错；停下时要到续传数据，从会话里移除并删掉 `.parts`。
pub(crate) async fn seed(
    seeder: Seeder,
    mut seeding: watch::Receiver<Seeding>,
    cancel: CancellationToken,
    mut each: impl FnMut(TorrentProgress),
) -> Seeded {
    let mut owner = seeder.owner;
    let mut ticks = tokio::time::interval(STATUS_INTERVAL);
    let mut tracker_error = None;
    let end = loop {
        tokio::select! {
            () = cancel.cancelled() => break SeedEnd::Stopped,
            routed = owner.next() => match routed {
                Routed::Alert(alert) => match alert.kind {
                    AlertKind::FileError => break SeedEnd::Failed(format!("读文件出错：{}", alert.message)),
                    AlertKind::TorrentError => break SeedEnd::Failed(format!("种子出错：{}", alert.message)),
                    AlertKind::TrackerError => tracker_error = Some(alert.message),
                    _ => {}
                },
                Routed::Lost => break SeedEnd::Failed("BT 会话的事件丢了".to_owned()),
            },
            // 设置改了：下一次状态时按新的判
            _ = seeding.changed() => {}
            _ = ticks.tick() => {
                let status = match owner.session().status(&owner.id) {
                    Ok(status) => status,
                    Err(error) => break SeedEnd::Failed(owner.libtorrent("查状态")(error).to_string()),
                };
                let mut progress = TorrentProgress::of(&status);
                progress.tracker_error.clone_from(&tracker_error);
                if matches!(progress.phase, TorrentPhase::Checking { .. }) {
                    each(progress);
                    continue;
                }
                if !status.finished {
                    break SeedEnd::Incomplete;
                }
                progress.phase = TorrentPhase::Seeding;
                let done = *seeding.borrow_and_update();
                if done.reached(progress.uploaded, progress.wanted, progress.seeding_seconds) {
                    break SeedEnd::Reached;
                }
                each(progress);
            }
        }
    };
    let (resume, stopped) = stop(&mut owner, Removal::PartFile).await;
    let end = match (end, stopped) {
        (SeedEnd::Failed(message), _) => SeedEnd::Failed(message),
        (_, Err(failure)) => SeedEnd::Failed(failure.message),
        (end, Ok(())) => end,
    };
    Seeded { end, resume }
}

/// 运行到下完、出错或取消。
async fn drive(
    owner: &mut Owner,
    cancel: &CancellationToken,
    each: &mut impl FnMut(TorrentProgress),
) -> Outcome {
    let mut ticks = tokio::time::interval(STATUS_INTERVAL);
    // 连续几次状态凑不齐全部内容（见 SCARCE_TICKS）
    let mut short = 0;
    let mut tracker_error = None;
    loop {
        tokio::select! {
            () = cancel.cancelled() => return Outcome::Paused,
            routed = owner.next() => match routed {
                Routed::Alert(alert) => match alert.kind {
                    AlertKind::Finished => return finished(owner),
                    AlertKind::FileError => {
                        return Outcome::Failed(Failure {
                            kind: FailureKind::Disk,
                            message: format!("BT 下载读写文件出错：{}", alert.message),
                        });
                    }
                    AlertKind::TorrentError => {
                        return Outcome::Failed(Failure {
                            kind: FailureKind::Other,
                            message: format!("BT 种子出错：{}", alert.message),
                        });
                    }
                    AlertKind::TrackerError => tracker_error = Some(alert.message),
                    // 续传数据与磁盘对不上时 libtorrent 自己改为整体校验，照常运行
                    _ => {}
                },
                Routed::Lost => {
                    return Outcome::Failed(Failure {
                        kind: FailureKind::Other,
                        message: "BT 会话的事件丢了，这个任务的状态说不清；继续即可".to_owned(),
                    });
                }
            },
            _ = ticks.tick() => match owner.session().status(&owner.id) {
                Ok(status) => {
                    let mut progress = TorrentProgress::of(&status);
                    if progress.phase == TorrentPhase::Finishing {
                        return finished(owner);
                    }
                    let shortfall = progress.phase == TorrentPhase::Downloading
                        && progress.peers > 0
                        && progress.reachable < progress.pieces;
                    short = if shortfall { short + 1 } else { 0 };
                    progress.scarce = short >= SCARCE_TICKS;
                    progress.tracker_error.clone_from(&tracker_error);
                    each(progress);
                }
                Err(error) => {
                    return Outcome::Failed(Failure {
                        kind: FailureKind::Other,
                        message: owner.libtorrent("查状态")(error).to_string(),
                    });
                }
            },
        }
    }
}

/// 下完了：结果为要下的字节数。
fn finished(owner: &Owner) -> Outcome {
    match owner.session().status(&owner.id) {
        Ok(status) => Outcome::Finished(u64::try_from(status.total_wanted).expect("字节数不为负")),
        Err(error) => Outcome::Failed(Failure {
            kind: FailureKind::Other,
            message: owner.libtorrent("查状态")(error).to_string(),
        }),
    }
}

/// 停下：存下进度（[`snapshot`]），再从会话里移除（照 `removal` 连带删文件）并等它移除完。返回要存的续传数据与停下时
/// 出的问题。
async fn stop(owner: &mut Owner, removal: Removal) -> (Option<Resume>, Result<(), Failure>) {
    let (resume, mut problem) = snapshot(owner).await;
    if let Err(error) = owner.remove(removal).await {
        problem.get_or_insert(Failure {
            kind: FailureKind::Other,
            message: format!("BT 任务停下时出错：{error}"),
        });
    }
    (resume, problem.map_or(Ok(()), Err))
}

/// 存下进度：要续传数据、紧接着把缓存写到磁盘，等两者都回来。还没校验完已有的文件的（libtorrent 这时写出的续传数据里
/// 一块也没有，torrent.cpp 的 write_resume_data 按已校验到哪写），不要续传数据，存储里的照旧；这时也还没开始下，没有
/// 要写盘的。返回要存的续传数据（不要或没要到时为 None），与出的问题：写盘出错（[`FailureKind::Disk`]），或要不到、
/// 等不到结果。
async fn snapshot(owner: &mut Owner) -> (Option<Resume>, Option<Failure>) {
    let other = |error: Error| Failure {
        kind: FailureKind::Other,
        message: format!("BT 任务停下时出错：{error}"),
    };
    let mut problem = None;
    let mut resume = None;
    let checked = owner
        .session()
        .status(&owner.id)
        .map(|status| {
            matches!(
                status.state(),
                Some(State::Downloading | State::Finished | State::Seeding)
            )
        })
        .map_err(owner.libtorrent("查状态"));
    let requested = match checked {
        Ok(true) => owner
            .session()
            .save_resume_data(&owner.id)
            .and_then(|()| owner.session().flush(&owner.id))
            .map(|()| true)
            .map_err(owner.libtorrent("要续传数据")),
        Ok(false) => Ok(false),
        Err(error) => Err(error),
    };
    match requested {
        Err(error) => problem = Some(other(error)),
        Ok(false) => {}
        Ok(true) => {
            // 续传数据在写盘之前取样：写完的事件来了，取样时已有的块都已在磁盘上
            let mut data = None::<Option<Vec<u8>>>;
            let mut flushed = false;
            let mut write_error = None;
            while data.is_none() || !flushed {
                let step = owner
                    .step(
                        "续传数据与写完缓存",
                        |alert| {
                            matches!(
                                alert.kind,
                                AlertKind::ResumeData
                                    | AlertKind::ResumeDataFailed
                                    | AlertKind::CacheFlushed
                            )
                        },
                        |alert| {
                            if alert.kind == AlertKind::FileError {
                                write_error.get_or_insert_with(|| alert.message.clone());
                            }
                        },
                    )
                    .await;
                match step {
                    Ok(alert) => match alert.kind {
                        AlertKind::ResumeData => data = Some(Some(alert.data)),
                        AlertKind::ResumeDataFailed => data = Some(None),
                        _ => flushed = true,
                    },
                    Err(error) => {
                        problem = Some(other(error));
                        break;
                    }
                }
            }
            let trusted = flushed && write_error.is_none();
            if let Some(message) = write_error {
                problem.get_or_insert(Failure {
                    kind: FailureKind::Disk,
                    message: format!("BT 下载写文件出错：{message}"),
                });
            }
            resume = data.flatten().map(|data| Resume { data, trusted });
        }
    }
    (resume, problem)
}

/// 删掉停着的 BT 任务下了的文件：以暂停加入会话，再连同文件一起移除（libtorrent 删它建的文件、空了的目录与存不要的
/// 文件的部分块的 `.parts` 文件），之后删掉内容的根 `root` 里操作系统自己建的文件（[`OS_LITTER`]）与空了的目录。根还在
/// （里面有别的东西，或删除出错）时报 [`Error::Torrent`]，写明留下了什么与 libtorrent 报的错。
pub(crate) async fn delete(
    engine: Arc<Engine>,
    info_hash: &str,
    resume: &[u8],
    root: &Path,
) -> Result<(), Error> {
    let id = TorrentId::parse(info_hash)
        .map_err(|error| Error::Corrupt(format!("种子的编号：{error}")))?;
    let save_path = root
        .parent()
        .ok_or_else(|| Error::Corrupt(format!("内容的根没有所在目录：{}", root.display())))?;
    let mut owner = engine.own(&id)?;
    engine.settled(&id).await?;
    let options = AddOptions {
        trust_pieces: true,
        paused: true,
        upload_only: false,
    };
    owner
        .session()
        .add_resume(resume, &id, utf8(save_path)?, options)
        .map_err(owner.libtorrent("加入"))?;
    // 目录里有操作系统自己建的文件时 libtorrent 删不掉目录、报删除失败：那些文件下面一起删，以根还在不在为准
    let deleted = owner.remove(Removal::Files).await;
    drop(owner);
    let swept = root.to_path_buf();
    let left = tokio::task::spawn_blocking(move || sweep(&swept))
        .await
        .expect("清理目录不会 panic，运行时关闭时本任务也一起丢弃");
    if left.is_empty() {
        return Ok(());
    }
    let shown: Vec<String> = left
        .iter()
        .take(5)
        .map(|path| path.display().to_string())
        .collect();
    let more = if left.len() > shown.len() {
        format!(" 等 {} 项", left.len())
    } else {
        String::new()
    };
    let cause = deleted
        .err()
        .map_or_else(String::new, |error| format!("（{error}）"));
    Err(Error::Torrent(format!(
        "留下了 {}{more}{cause}",
        shown.join("、")
    )))
}

/// 删掉 `path`（BT 内容的根）下操作系统自己建的文件（[`OS_LITTER`]）与空了的目录，返回删不掉的（还在的文件、删除出错
/// 的）。`path` 已不在时为空；符号链接不跟进，当作文件。
fn sweep(path: &Path) -> Vec<PathBuf> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        // 不在了；在而读不了属性的，下面删的时候报出来
        return if path.exists() {
            vec![path.to_path_buf()]
        } else {
            Vec::new()
        };
    };
    if !meta.is_dir() {
        let litter = path
            .file_name()
            .is_some_and(|name| OS_LITTER.iter().any(|litter| name == *litter));
        return if litter && std::fs::remove_file(path).is_ok() {
            Vec::new()
        } else {
            vec![path.to_path_buf()]
        };
    }
    let mut left = Vec::new();
    match std::fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) => left.extend(sweep(&entry.path())),
                    Err(_) => left.push(path.to_path_buf()),
                }
            }
        }
        Err(_) => left.push(path.to_path_buf()),
    }
    if left.is_empty() && std::fs::remove_dir(path).is_err() {
        left.push(path.to_path_buf());
    }
    left
}

fn utf8(path: &Path) -> Result<&str, Error> {
    path.to_str()
        .ok_or_else(|| Error::InvalidInput(format!("路径不是 UTF-8：{}", path.display())))
}

/// 加锁；持锁期间只读写表，不会 panic，锁中毒即不变量被破坏。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("持锁期间不会 panic")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_take_one_digit_per_part() {
        assert_eq!(fingerprint("0.2.0"), "-HD0200-");
        assert_eq!(fingerprint("1.12.3"), "-HD1930-");
    }
}
