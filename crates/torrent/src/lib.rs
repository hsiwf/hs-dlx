//! BT 引擎：静态链接的 libtorrent-rasterbar（2.1）上的一层很薄的绑定，只公开桌面应用要用的会话操作。C++ 的那一层在
//! src/shim.cpp，库由 third_party/libtorrent/build.sh 编出。
//!
//! 种子用信息哈希的十六进制串（[`TorrentId`]）指认。会话的操作都交给 libtorrent 的网络线程去做，可以在任意线程上调用；
//! 事件（[`Alert`]）由 [`Session::pop_alerts`] 取出，取出时已转成自有的数据。

use std::fmt;

#[cxx::bridge(namespace = "hsdlx")]
mod ffi {
    /// 会话的设置。
    struct SessionConfig {
        /// 监听的地址与端口，libtorrent 的写法（如 `0.0.0.0:6881,[::]:6881`；端口为 0 时由系统挑）
        listen_interfaces: String,
        dht: bool,
        upnp: bool,
        natpmp: bool,
        /// 本地网络里的节点发现
        lsd: bool,
        /// 连 tracker 与 web seed 时的 User-Agent
        user_agent: String,
        /// 节点 ID 的前缀，标明客户端（如 `-HD0100-`）
        peer_fingerprint: String,
    }

    /// 一个种子此刻的状态。
    struct Status {
        /// libtorrent 的 `torrent_status::state_t`，见 [`crate::State`]
        state: u8,
        /// 已取得种子信息（磁力链接要先从节点那里取）
        has_metadata: bool,
        paused: bool,
        name: String,
        /// 要下载的字节数（没选的文件不算）；没有种子信息时为 0
        total_wanted: i64,
        /// 其中已下完并校验过的
        total_wanted_done: i64,
        /// 下载与上传的速度，字节每秒，只算内容
        download_rate: i32,
        upload_rate: i32,
        /// 连着的节点数，其中的做种者
        peers: i32,
        seeds: i32,
        /// 总块数；没有种子信息时为 0
        pieces: i32,
        /// 能凑齐的块数：自己已有的，或在线的节点里至少有一个有的
        reachable_pieces: i32,
        /// 种子出错的原因；没出错时为空
        error: String,
    }

    /// 事件的种类。
    enum AlertKind {
        Added,
        AddFailed,
        /// 磁力链接取到了种子信息
        Metadata,
        /// 选了的文件都下完了
        Finished,
        /// [`Session::save_resume_data`] 的结果：`data` 为续传数据
        ResumeData,
        ResumeDataFailed,
        TorrentError,
        FileError,
        Removed,
        ListenSucceeded,
        ListenFailed,
        PortMapped,
        PortMapFailed,
    }

    /// 一个事件。
    struct Alert {
        kind: AlertKind,
        /// 关于哪个种子（[`crate::TorrentId`] 的写法）；与种子无关的为空
        torrent: String,
        /// libtorrent 的说明，英文
        message: String,
        /// 续传数据（[`AlertKind::ResumeData`]）；别的为空
        data: Vec<u8>,
    }

    unsafe extern "C++" {
        include!("hs-dlx-torrent/src/shim.h");

        type Session;

        fn new_session(config: &SessionConfig) -> Result<UniquePtr<Session>>;
        fn add_magnet(self: &Session, uri: &str, save_path: &str) -> Result<String>;
        fn add_torrent(self: &Session, torrent: &[u8], save_path: &str) -> Result<String>;
        fn status(self: &Session, id: &str) -> Result<Status>;
        fn connect_peer(self: &Session, id: &str, ip: &str, port: u16) -> Result<()>;
        fn pause(self: &Session, id: &str) -> Result<()>;
        fn resume(self: &Session, id: &str) -> Result<()>;
        fn remove(self: &Session, id: &str, delete_files: bool) -> Result<()>;
        fn save_resume_data(self: &Session, id: &str) -> Result<()>;
        fn listen_port(self: &Session) -> u16;
        fn pop_alerts(self: &Session, wait_ms: u32) -> Vec<Alert>;
        fn create_torrent(path: &str, piece_size: i32) -> Result<Vec<u8>>;
    }
}

pub use ffi::{Alert, AlertKind, SessionConfig, Status};

// SAFETY: libtorrent 的会话把每个操作作为消息交给它自己的网络线程执行（docs/hacking.rst：「manipulates it by sending
// messages to the network thread」）；C++ 那一层自己的状态（种子表、取事件）各有一把锁。
unsafe impl Send for ffi::Session {}
// SAFETY: 同上，`&Session` 上的操作在多个线程上同时调用也只是各发各的消息、各自加锁。
unsafe impl Sync for ffi::Session {}

/// 种子的指认：信息哈希的十六进制串（v1 的 SHA-1；只有 v2 时为截到 20 字节的 SHA-256）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TorrentId(String);

impl TorrentId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TorrentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// libtorrent 报的错误（C++ 抛出的异常），说明为英文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<cxx::Exception> for Error {
    fn from(exception: cxx::Exception) -> Self {
        Error(exception.what().to_owned())
    }
}

/// 种子在做什么（libtorrent 的 `torrent_status::state_t`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// 校验已有的文件
    CheckingFiles,
    /// 磁力链接在向节点取种子信息
    DownloadingMetadata,
    Downloading,
    /// 选了的文件下完了，没做种
    Finished,
    Seeding,
    /// 校验续传数据
    CheckingResumeData,
}

impl Status {
    /// libtorrent 报的状态；它新加了别的取值时为 None。
    pub fn state(&self) -> Option<State> {
        Some(match self.state {
            1 => State::CheckingFiles,
            2 => State::DownloadingMetadata,
            3 => State::Downloading,
            4 => State::Finished,
            5 => State::Seeding,
            7 => State::CheckingResumeData,
            _ => return None,
        })
    }
}

/// 一个 BT 会话：监听端口、连节点、管理加进来的种子。释放时等 libtorrent 关好会话（向 tracker 报告停止，最多几秒）。
pub struct Session(cxx::UniquePtr<ffi::Session>);

impl Session {
    pub fn new(config: &SessionConfig) -> Result<Session, Error> {
        Ok(Session(ffi::new_session(config)?))
    }

    fn inner(&self) -> &ffi::Session {
        self.0.as_ref().expect("new_session 成功时返回的指针不为空")
    }

    /// 加磁力链接，下到 `save_path` 下（种子里的文件与目录建在它里面）。
    pub fn add_magnet(&self, uri: &str, save_path: &str) -> Result<TorrentId, Error> {
        Ok(TorrentId(self.inner().add_magnet(uri, save_path)?))
    }

    /// 加种子文件（bencode 写的内容），下到 `save_path` 下。
    pub fn add_torrent(&self, torrent: &[u8], save_path: &str) -> Result<TorrentId, Error> {
        Ok(TorrentId(self.inner().add_torrent(torrent, save_path)?))
    }

    pub fn status(&self, id: &TorrentId) -> Result<Status, Error> {
        Ok(self.inner().status(id.as_str())?)
    }

    /// 主动连一个节点（`ip` 为 IPv4 或 IPv6 的写法）。
    pub fn connect_peer(&self, id: &TorrentId, ip: &str, port: u16) -> Result<(), Error> {
        Ok(self.inner().connect_peer(id.as_str(), ip, port)?)
    }

    /// 暂停：在途的块下完再停。
    pub fn pause(&self, id: &TorrentId) -> Result<(), Error> {
        Ok(self.inner().pause(id.as_str())?)
    }

    pub fn resume(&self, id: &TorrentId) -> Result<(), Error> {
        Ok(self.inner().resume(id.as_str())?)
    }

    /// 从会话里去掉；`delete_files` 时连同下了的文件一起删（删完报 [`AlertKind::Removed`] 之后的文件事件）。
    pub fn remove(&self, id: &TorrentId, delete_files: bool) -> Result<(), Error> {
        Ok(self.inner().remove(id.as_str(), delete_files)?)
    }

    /// 要求写出续传数据，结果以 [`AlertKind::ResumeData`] 或 [`AlertKind::ResumeDataFailed`] 报来。
    pub fn save_resume_data(&self, id: &TorrentId) -> Result<(), Error> {
        Ok(self.inner().save_resume_data(id.as_str())?)
    }

    /// 实际监听的端口；还没监听成功时为 0。
    pub fn listen_port(&self) -> u16 {
        self.inner().listen_port()
    }

    /// 取出积攒的事件；没有时最多等 `wait_ms` 毫秒。
    pub fn pop_alerts(&self, wait_ms: u32) -> Vec<Alert> {
        self.inner().pop_alerts(wait_ms)
    }
}

/// 为 `path`（文件或目录）建种子文件，分块大小 `piece_size` 字节（0 为按总大小自动选）；返回 bencode 写出的内容。
pub fn create_torrent(path: &str, piece_size: i32) -> Result<Vec<u8>, Error> {
    Ok(ffi::create_torrent(path, piece_size)?)
}

impl fmt::Debug for Alert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Alert")
            .field("kind", &self.kind)
            .field("torrent", &self.torrent)
            .field("message", &self.message)
            .field("data", &self.data.len())
            .finish()
    }
}

impl fmt::Debug for AlertKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AlertKind({})", self.repr)
    }
}
