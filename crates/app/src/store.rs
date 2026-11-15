//! 任务存储：一个 SQLite 文件。表结构的版本记在 `PRAGMA user_version`，打开时按版本迁移；读写函数接受调用方
//! 开好的事务，事务边界由服务层定。

use std::collections::HashSet;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use hs_m3u8_core::{Proxy, Url};

use crate::preference::StoredPreference;
use crate::torrent::{Resume, SessionState};
use crate::{
    Error, Expected, Failure, FailureKind, FileSpec, Finished, HlsSpec, Kind, OutputSpec, Settings,
    Status, Task, TaskId, TaskSpec, Theme, TorrentSpec, naming,
};

/// 一次迁移：在给定的事务里把表结构升一个版本。
type Migration = fn(&Transaction<'_>) -> Result<(), Error>;

/// 各版本的迁移：第 i 项把版本 i 升到 i + 1。已发布的迁移不再改动，新的表结构只追加一项。
const MIGRATIONS: [Migration; 7] = [
    create_task_table,
    rebuild_with_work_dir_and_result,
    create_settings_table,
    rebuild_with_kinds,
    add_theme,
    rebuild_with_ranges,
    rebuild_with_torrents,
];

fn create_task_table(tx: &Transaction<'_>) -> Result<(), Error> {
    tx.execute_batch(
        "CREATE TABLE task (
            id INTEGER PRIMARY KEY,
            title TEXT NOT NULL,
            url TEXT NOT NULL,
            preference TEXT NOT NULL,
            headers TEXT NOT NULL,
            output_kind TEXT NOT NULL CHECK (output_kind IN ('mp4', 'hls', 'both')),
            mp4_path TEXT,
            hls_path TEXT,
            live INTEGER NOT NULL CHECK (live IN (0, 1)),
            status TEXT NOT NULL CHECK (status IN ('running', 'paused', 'failed', 'done')),
            error TEXT,
            created_at INTEGER NOT NULL
        ) STRICT;",
    )?;
    Ok(())
}

/// 第 2 版，重建表：
/// - 编号不再重用（AUTOINCREMENT）：界面按编号认领异步回来的结果，删掉的任务的编号给了新任务就会认错。
/// - 任务目录单列（绝对路径；NULL 时任务没有自己的任务目录）。第 1 版的行按第 1 版的规则补算：没完成的取与输出同级的
///   `<主名>.hsdl`，已完成的为 NULL（任务目录已在完成时删掉）。补算出的任务目录相同（按 [`v2_path_key`] 比较）的一组
///   里，编号最小的留着它，其余的改为失败、任务目录为 NULL：放弃它们会删掉留着的那个已下的内容。
/// - 失败的种类（[`crate::FailureKind`] 在存储里的写法，CHECK 列出全部写法），只在失败时有；第 1 版失败的行记为
///   其余。
/// - 点播的总片数，与完成时写出的结果（时长、字节数、视频的宽高）。
/// - 继续时只写出已录的（[`Task::write_only`]），第 1 版的行为否。
fn rebuild_with_work_dir_and_result(tx: &Transaction<'_>) -> Result<(), Error> {
    tx.execute_batch(
        "CREATE TABLE task_v2 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            url TEXT NOT NULL,
            preference TEXT NOT NULL,
            headers TEXT NOT NULL,
            output_kind TEXT NOT NULL CHECK (output_kind IN ('mp4', 'hls', 'both')),
            mp4_path TEXT,
            hls_path TEXT,
            work_dir TEXT,
            live INTEGER NOT NULL CHECK (live IN (0, 1)),
            status TEXT NOT NULL CHECK (status IN ('running', 'paused', 'failed', 'done', 'discard-unfinished')),
            error TEXT,
            failure TEXT CHECK (
                (status = 'failed') = (failure IS NOT NULL)
                AND (failure IS NULL
                     OR failure IN ('network', 'certificate', 'disk', 'output-taken', 'source-changed', 'malformed',
                                    'stalled', 'nothing-to-write', 'mp4-unsupported', 'unsupported', 'unrecoverable',
                                    'other')
                     OR failure GLOB 'status:[0-9][0-9][0-9]')
            ),
            created_at INTEGER NOT NULL,
            total INTEGER CHECK (total >= 0),
            duration_ms INTEGER CHECK (duration_ms >= 0),
            bytes INTEGER CHECK (bytes >= 0),
            width INTEGER CHECK (width > 0),
            height INTEGER CHECK (height > 0),
            write_only INTEGER NOT NULL DEFAULT 0 CHECK (write_only IN (0, 1))
        ) STRICT;
        INSERT INTO task_v2 (id, title, url, preference, headers, output_kind, mp4_path, hls_path, live, status,
                             error, failure, created_at)
            SELECT id, title, url, preference, headers, output_kind, mp4_path, hls_path, live, status, error,
                   CASE WHEN status = 'failed' THEN 'other' END, created_at
            FROM task;
        DROP TABLE task;
        ALTER TABLE task_v2 RENAME TO task;",
    )?;
    /// 第 1 版的一行里迁移要用的列。
    struct V1Row {
        id: i64,
        title: String,
        status: String,
        kind: String,
        mp4: Option<String>,
        hls: Option<String>,
    }
    let rows: Vec<V1Row> = tx
        .prepare("SELECT id, title, status, output_kind, mp4_path, hls_path FROM task ORDER BY id")?
        .query_map([], |row| {
            Ok(V1Row {
                id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                kind: row.get(3)?,
                mp4: row.get(4)?,
                hls: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    // 任务目录的比较键 → 留着它的任务的标题
    let mut owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for row in rows {
        let work_dir = match (row.kind.as_str(), row.mp4, row.hls) {
            ("mp4" | "both", Some(mp4), _) => v1_work_dir(&mp4, true),
            ("hls", None, Some(hls)) => v1_work_dir(&hls, false),
            _ => None,
        }
        .ok_or_else(|| Error::Corrupt(format!("任务 {} 的输出", row.id)))?;
        if row.status == "done" {
            continue;
        }
        match owners.entry(v2_path_key(&work_dir)) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(row.title);
                tx.execute(
                    "UPDATE task SET work_dir = ?2 WHERE id = ?1",
                    params![row.id, work_dir],
                )?;
            }
            std::collections::hash_map::Entry::Occupied(owner) => {
                tx.execute(
                    "UPDATE task SET status = 'failed', error = ?2, failure = 'other' WHERE id = ?1",
                    params![
                        row.id,
                        format!(
                            "与「{}」共用任务目录 {work_dir}，放弃这个任务会删掉那个任务已下的内容；只能从列表移除",
                            owner.get()
                        )
                    ],
                )?;
            }
        }
    }
    Ok(())
}

/// 第 3 版：设置（[`crate::Settings`]）只有一行，编号固定为 1；保存目录为 NULL 时没设过，代理的写法见
/// [`proxy_code`]。
fn create_settings_table(tx: &Transaction<'_>) -> Result<(), Error> {
    tx.execute_batch(
        "CREATE TABLE settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            folder TEXT,
            proxy TEXT NOT NULL DEFAULT 'system' CHECK (proxy IN ('system', 'direct') OR proxy GLOB 'url:?*')
        ) STRICT;
        INSERT INTO settings (id) VALUES (1);",
    )?;
    Ok(())
}

/// 第 4 版，重建表，任务分种类（`kind`）：
/// - `hls`：选轨偏好、输出（种类与 MP4、本地 HLS 的路径）、是否录直播必有，总片数与只写出已录只用于它。
/// - `file`：单个文件，输出路径必有；认出地址时服务器报的总字节数、ETag、Last-Modified、Content-Type（第一次下载按它们
///   核对，都可无），与每个请求最多取的字节数（可无）只用于它。
///
/// 第 3 版的行都是 `hls`。失败种类的写法不变。删掉旧表会连同删掉它的编号计数，新表的计数接着旧表的，编号照样不重用。
fn rebuild_with_kinds(tx: &Transaction<'_>) -> Result<(), Error> {
    let counter = task_counter(tx)?;
    tx.execute_batch(
        "CREATE TABLE task_v4 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL CHECK (kind IN ('hls', 'file')),
            title TEXT NOT NULL,
            url TEXT NOT NULL,
            headers TEXT NOT NULL,
            preference TEXT,
            output_kind TEXT CHECK (output_kind IN ('mp4', 'hls', 'both')),
            mp4_path TEXT,
            hls_path TEXT,
            live INTEGER CHECK (live IN (0, 1)),
            file_path TEXT,
            file_length INTEGER CHECK (file_length >= 0),
            file_etag TEXT,
            file_last_modified TEXT,
            file_content_type TEXT,
            chunk_size INTEGER CHECK (chunk_size > 0),
            work_dir TEXT,
            status TEXT NOT NULL CHECK (status IN ('running', 'paused', 'failed', 'done', 'discard-unfinished')),
            error TEXT,
            failure TEXT CHECK (
                (status = 'failed') = (failure IS NOT NULL)
                AND (failure IS NULL
                     OR failure IN ('network', 'certificate', 'disk', 'output-taken', 'source-changed', 'malformed',
                                    'stalled', 'nothing-to-write', 'mp4-unsupported', 'unsupported', 'unrecoverable',
                                    'other')
                     OR failure GLOB 'status:[0-9][0-9][0-9]')
            ),
            created_at INTEGER NOT NULL,
            total INTEGER CHECK (total >= 0),
            duration_ms INTEGER CHECK (duration_ms >= 0),
            bytes INTEGER CHECK (bytes >= 0),
            width INTEGER CHECK (width > 0),
            height INTEGER CHECK (height > 0),
            write_only INTEGER NOT NULL DEFAULT 0 CHECK (write_only IN (0, 1)),
            CHECK (kind = 'file' OR (preference IS NOT NULL AND output_kind IS NOT NULL AND live IS NOT NULL)),
            CHECK (kind = 'hls' OR (preference IS NULL AND output_kind IS NULL AND live IS NULL AND mp4_path IS NULL
                                    AND hls_path IS NULL AND total IS NULL AND write_only = 0)),
            CHECK ((kind = 'file') = (file_path IS NOT NULL)),
            CHECK (kind = 'file' OR (file_length IS NULL AND file_etag IS NULL AND file_last_modified IS NULL
                                     AND file_content_type IS NULL AND chunk_size IS NULL))
        ) STRICT;
        INSERT INTO task_v4 (id, kind, title, url, headers, preference, output_kind, mp4_path, hls_path, live, work_dir,
                             status, error, failure, created_at, total, duration_ms, bytes, width, height, write_only)
            SELECT id, 'hls', title, url, headers, preference, output_kind, mp4_path, hls_path, live, work_dir, status,
                   error, failure, created_at, total, duration_ms, bytes, width, height, write_only
            FROM task;
        DROP TABLE task;
        ALTER TABLE task_v4 RENAME TO task;",
    )?;
    keep_task_counter(tx, counter)
}

/// 任务表的编号计数（AUTOINCREMENT 记在 sqlite_sequence 里）；还没分配过编号时为 None。重建任务表前读出，重建后由
/// [`keep_task_counter`] 接上：删掉旧表会连同删掉它的计数。
fn task_counter(tx: &Transaction<'_>) -> Result<Option<i64>, Error> {
    Ok(tx
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'task'",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

/// 重建后的任务表的编号计数接着重建前的 `counter`（[`task_counter`]）。
fn keep_task_counter(tx: &Transaction<'_>, counter: Option<i64>) -> Result<(), Error> {
    if let Some(counter) = counter {
        let kept = tx.execute(
            "UPDATE sqlite_sequence SET seq = max(seq, ?1) WHERE name = 'task'",
            params![counter],
        )?;
        if kept == 0 {
            tx.execute(
                "INSERT INTO sqlite_sequence (name, seq) VALUES ('task', ?1)",
                params![counter],
            )?;
        }
    }
    Ok(())
}

/// 第 6 版，重建任务表：
/// - 文件任务加 `file_accepts_ranges`（认出地址时服务器支持范围请求，第一次下载按它决定带不带 If-Range 核对），只用于
///   文件任务且必有。第 5 版没有文件任务（服务拒绝新建）；有的话照下面只拷了两种任务共有的列，文件的各列都补不出，
///   迁移按约束失败、存储留在第 5 版。
/// - 失败的种类加 `not-expected`（[`crate::FailureKind::NotExpected`]）。
fn rebuild_with_ranges(tx: &Transaction<'_>) -> Result<(), Error> {
    let counter = task_counter(tx)?;
    tx.execute_batch(
        "CREATE TABLE task_v6 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL CHECK (kind IN ('hls', 'file')),
            title TEXT NOT NULL,
            url TEXT NOT NULL,
            headers TEXT NOT NULL,
            preference TEXT,
            output_kind TEXT CHECK (output_kind IN ('mp4', 'hls', 'both')),
            mp4_path TEXT,
            hls_path TEXT,
            live INTEGER CHECK (live IN (0, 1)),
            file_path TEXT,
            file_length INTEGER CHECK (file_length >= 0),
            file_accepts_ranges INTEGER CHECK (file_accepts_ranges IN (0, 1)),
            file_etag TEXT,
            file_last_modified TEXT,
            file_content_type TEXT,
            chunk_size INTEGER CHECK (chunk_size > 0),
            work_dir TEXT,
            status TEXT NOT NULL CHECK (status IN ('running', 'paused', 'failed', 'done', 'discard-unfinished')),
            error TEXT,
            failure TEXT CHECK (
                (status = 'failed') = (failure IS NOT NULL)
                AND (failure IS NULL
                     OR failure IN ('network', 'certificate', 'disk', 'output-taken', 'source-changed', 'not-expected',
                                    'malformed', 'stalled', 'nothing-to-write', 'mp4-unsupported', 'unsupported',
                                    'unrecoverable', 'other')
                     OR failure GLOB 'status:[0-9][0-9][0-9]')
            ),
            created_at INTEGER NOT NULL,
            total INTEGER CHECK (total >= 0),
            duration_ms INTEGER CHECK (duration_ms >= 0),
            bytes INTEGER CHECK (bytes >= 0),
            width INTEGER CHECK (width > 0),
            height INTEGER CHECK (height > 0),
            write_only INTEGER NOT NULL DEFAULT 0 CHECK (write_only IN (0, 1)),
            CHECK (kind = 'file' OR (preference IS NOT NULL AND output_kind IS NOT NULL AND live IS NOT NULL)),
            CHECK (kind = 'hls' OR (preference IS NULL AND output_kind IS NULL AND live IS NULL AND mp4_path IS NULL
                                    AND hls_path IS NULL AND total IS NULL AND write_only = 0)),
            CHECK ((kind = 'file') = (file_path IS NOT NULL AND file_accepts_ranges IS NOT NULL)),
            CHECK (kind = 'file' OR (file_path IS NULL AND file_accepts_ranges IS NULL AND file_length IS NULL
                                     AND file_etag IS NULL AND file_last_modified IS NULL AND file_content_type IS NULL
                                     AND chunk_size IS NULL))
        ) STRICT;
        INSERT INTO task_v6 (id, kind, title, url, headers, preference, output_kind, mp4_path, hls_path, live, work_dir,
                             status, error, failure, created_at, total, duration_ms, bytes, width, height, write_only)
            SELECT id, kind, title, url, headers, preference, output_kind, mp4_path, hls_path, live, work_dir, status,
                   error, failure, created_at, total, duration_ms, bytes, width, height, write_only
            FROM task;
        DROP TABLE task;
        ALTER TABLE task_v6 RENAME TO task;",
    )?;
    keep_task_counter(tx, counter)
}

/// 第 7 版：
/// - 重建任务表，加种类 `torrent`（BT）：种子的编号（信息哈希的十六进制，40 个小写字母与数字，不同任务不重复）、内容的
///   根（绝对路径）、续传数据（libtorrent 的格式，含种子信息）、续传数据记的已有块是否可信（见
///   [`crate::torrent::Resume::trusted`]）四列必有且只用于它；BT 任务没有任务目录与请求头（`[]`）。HLS 与文件的约束
///   不变。
/// - 设置加 BT 会话的监听端口（1 到 65535，没监听过为 NULL）与 DHT 状态（libtorrent 的格式，没存过为 NULL）。
fn rebuild_with_torrents(tx: &Transaction<'_>) -> Result<(), Error> {
    let counter = task_counter(tx)?;
    tx.execute_batch(
        "CREATE TABLE task_v7 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL CHECK (kind IN ('hls', 'file', 'torrent')),
            title TEXT NOT NULL,
            url TEXT NOT NULL,
            headers TEXT NOT NULL,
            preference TEXT,
            output_kind TEXT CHECK (output_kind IN ('mp4', 'hls', 'both')),
            mp4_path TEXT,
            hls_path TEXT,
            live INTEGER CHECK (live IN (0, 1)),
            file_path TEXT,
            file_length INTEGER CHECK (file_length >= 0),
            file_accepts_ranges INTEGER CHECK (file_accepts_ranges IN (0, 1)),
            file_etag TEXT,
            file_last_modified TEXT,
            file_content_type TEXT,
            chunk_size INTEGER CHECK (chunk_size > 0),
            torrent_info_hash TEXT UNIQUE CHECK (length(torrent_info_hash) = 40
                                                 AND torrent_info_hash NOT GLOB '*[^0-9a-f]*'),
            torrent_root TEXT,
            torrent_resume BLOB,
            torrent_trusted INTEGER CHECK (torrent_trusted IN (0, 1)),
            work_dir TEXT,
            status TEXT NOT NULL CHECK (status IN ('running', 'paused', 'failed', 'done', 'discard-unfinished')),
            error TEXT,
            failure TEXT CHECK (
                (status = 'failed') = (failure IS NOT NULL)
                AND (failure IS NULL
                     OR failure IN ('network', 'certificate', 'disk', 'output-taken', 'source-changed', 'not-expected',
                                    'malformed', 'stalled', 'nothing-to-write', 'mp4-unsupported', 'unsupported',
                                    'unrecoverable', 'other')
                     OR failure GLOB 'status:[0-9][0-9][0-9]')
            ),
            created_at INTEGER NOT NULL,
            total INTEGER CHECK (total >= 0),
            duration_ms INTEGER CHECK (duration_ms >= 0),
            bytes INTEGER CHECK (bytes >= 0),
            width INTEGER CHECK (width > 0),
            height INTEGER CHECK (height > 0),
            write_only INTEGER NOT NULL DEFAULT 0 CHECK (write_only IN (0, 1)),
            CHECK (kind != 'hls' OR (preference IS NOT NULL AND output_kind IS NOT NULL AND live IS NOT NULL)),
            CHECK (kind = 'hls' OR (preference IS NULL AND output_kind IS NULL AND live IS NULL AND mp4_path IS NULL
                                    AND hls_path IS NULL AND total IS NULL AND write_only = 0)),
            CHECK ((kind = 'file') = (file_path IS NOT NULL AND file_accepts_ranges IS NOT NULL)),
            CHECK (kind = 'file' OR (file_path IS NULL AND file_accepts_ranges IS NULL AND file_length IS NULL
                                     AND file_etag IS NULL AND file_last_modified IS NULL AND file_content_type IS NULL
                                     AND chunk_size IS NULL)),
            CHECK ((kind = 'torrent') = (torrent_info_hash IS NOT NULL AND torrent_root IS NOT NULL
                                         AND torrent_resume IS NOT NULL AND torrent_trusted IS NOT NULL)),
            CHECK (kind = 'torrent' OR (torrent_info_hash IS NULL AND torrent_root IS NULL AND torrent_resume IS NULL
                                        AND torrent_trusted IS NULL)),
            CHECK (kind != 'torrent' OR (work_dir IS NULL AND headers = '[]'))
        ) STRICT;
        INSERT INTO task_v7 (id, kind, title, url, headers, preference, output_kind, mp4_path, hls_path, live, file_path,
                             file_length, file_accepts_ranges, file_etag, file_last_modified, file_content_type,
                             chunk_size, work_dir, status, error, failure, created_at, total, duration_ms, bytes, width,
                             height, write_only)
            SELECT id, kind, title, url, headers, preference, output_kind, mp4_path, hls_path, live, file_path,
                   file_length, file_accepts_ranges, file_etag, file_last_modified, file_content_type, chunk_size,
                   work_dir, status, error, failure, created_at, total, duration_ms, bytes, width, height, write_only
            FROM task;
        DROP TABLE task;
        ALTER TABLE task_v7 RENAME TO task;
        ALTER TABLE settings ADD COLUMN torrent_port INTEGER CHECK (torrent_port BETWEEN 1 AND 65535);
        ALTER TABLE settings ADD COLUMN torrent_dht BLOB;",
    )?;
    keep_task_counter(tx, counter)
}

/// 第 2 版迁移比较路径用的键：转小写再做 NFC，与写这一版时的 [`naming::path_key`] 相同；那边之后改规则不影响这次
/// 迁移的结果。
fn v2_path_key(path: &str) -> String {
    icu_normalizer::ComposingNormalizerBorrowed::new_nfc()
        .normalize(&path.to_lowercase())
        .into_owned()
}

/// 第 1 版任务目录的位置：输出 `path`（绝对路径）同级的 `<主名>.hsdl`，MP4 的主名去掉扩展名、本地 HLS 的为目录名；
/// 路径不以文件名结尾时为 None。只给第 2 版的迁移用，与当前的规则无关。
fn v1_work_dir(path: &str, strip_extension: bool) -> Option<String> {
    let path = Path::new(path);
    let name = if strip_extension {
        path.file_stem()?
    } else {
        path.file_name()?
    };
    let mut dir = name.to_os_string();
    dir.push(".hsdl");
    path.parent()?.join(dir).into_os_string().into_string().ok()
}

/// 存储被别的连接占着时等多久再报错：有单实例锁，占着它的只会是外部工具（如 sqlite3 命令行），而写存储常在界面线程上。
const BUSY_TIMEOUT: Duration = Duration::from_secs(1);

/// 打开的任务存储。
pub struct Store {
    conn: Connection,
    /// 文件存储旁的锁文件，持有期间别的进程打不开同一份存储；内存里的存储为 None
    _lock: Option<File>,
}

impl Store {
    /// 打开（不存在时新建）`path` 处的存储并迁移到当前版本；所在目录不存在时创建。Unix 上文件只有本人可读写：
    /// 里面明文存着请求头。版本比本应用认识的新时报 [`Error::Corrupt`]（用新版本的应用打开过）。同一份存储同时只能
    /// 被一个进程打开：先对同目录的 `<文件名>.lock` 加排他锁，已被别的进程锁着时报 [`Error::InUse`]；锁随存储释放，
    /// 进程退出时由系统释放。
    pub fn open(path: &Path) -> Result<Store, Error> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io("创建", dir))?;
        }
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);
        let lock = File::create(&lock_path).map_err(io("创建", &lock_path))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(Error::InUse(path.to_path_buf())),
            Err(TryLockError::Error(cause)) => return Err(io("加锁", &lock_path)(cause)),
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // 请求头里有明文的 Cookie、Authorization：删掉或换下来的内容随即覆盖，不留在空闲页里
        conn.pragma_update(None, "secure_delete", true)?;
        restrict_to_owner(path)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let mut store = Store {
            conn,
            _lock: Some(lock),
        };
        store.migrate()?;
        Ok(store)
    }

    /// 内存里的存储，用于测试。
    pub fn in_memory() -> Result<Store, Error> {
        let mut store = Store {
            conn: Connection::open_in_memory()?,
            _lock: None,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn transaction(&mut self) -> Result<Transaction<'_>, Error> {
        Ok(self.conn.transaction()?)
    }

    fn migrate(&mut self) -> Result<(), Error> {
        let tx = self.conn.transaction()?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let version = usize::try_from(version)
            .map_err(|_| Error::Corrupt(format!("表结构版本 {version} 不是非负数")))?;
        if version > MIGRATIONS.len() {
            return Err(Error::Corrupt(format!(
                "表结构版本 {version} 比本应用认识的 {} 新",
                MIGRATIONS.len()
            )));
        }
        for migration in &MIGRATIONS[version..] {
            migration(&tx)?;
        }
        tx.pragma_update(None, "user_version", schema_version(MIGRATIONS.len()))?;
        tx.commit()?;
        Ok(())
    }
}

/// 读出一个任务的各列，顺序与 [`Row::read`] 一致。
const COLUMNS: &str = "id, kind, title, url, headers, preference, output_kind, mp4_path, hls_path, live, file_path, \
                       file_length, file_etag, file_last_modified, file_content_type, chunk_size, work_dir, status, \
                       error, failure, created_at, total, duration_ms, bytes, width, height, write_only, \
                       file_accepts_ranges, torrent_info_hash, torrent_root";

/// 新建任务，状态为运行中，任务目录为 `work_dir`（绝对路径；BT 任务没有，为 None），BT 任务的续传数据为 `resume`
/// （别的任务为 None）；返回分配的编号。输出或任务目录与已有任务的输出或任务目录相同（按 [`naming::path_key`] 比较）
/// 时报 [`Error::PathTaken`]：共用任务目录时，放弃其中一个会删掉另一个已下的内容；共用输出时，后写出的那个要到写出时
/// 才失败。BT 任务的种子已在别的任务里时报 [`Error::TorrentTaken`]。
pub fn insert(
    tx: &Transaction<'_>,
    spec: &TaskSpec,
    work_dir: Option<&Path>,
    resume: Option<&[u8]>,
    created_at: i64,
) -> Result<TaskId, Error> {
    let work_dir_text = work_dir.map(utf8).transpose()?;
    if let Some(dir) = work_dir
        && !dir.is_absolute()
    {
        return Err(Error::InvalidInput(format!(
            "任务目录不是绝对路径：{}",
            dir.display()
        )));
    }
    // 只属于别的种类的列为 NULL（表上的 CHECK 按种类核对）
    let (preference, output_kind, mp4, hls, live) = match &spec.kind {
        Kind::Hls(spec) => {
            let (kind, mp4, hls) = output_columns(&spec.output)?;
            (
                Some(to_json(&spec.preference)),
                Some(kind),
                mp4,
                hls,
                Some(spec.live),
            )
        }
        Kind::File(_) | Kind::Torrent(_) => (None, None, None, None, None),
    };
    let (file_path, length, accepts_ranges, etag, last_modified, content_type, chunk) = match &spec
        .kind
    {
        Kind::Hls(_) | Kind::Torrent(_) => (None, None, None, None, None, None, None),
        Kind::File(file) => (
            Some(named_utf8(&file.path)?),
            // 服务器报的，可能大得离谱
            file.expected
                .length
                .map(|length| {
                    i64::try_from(length).map_err(|_| {
                        Error::InvalidInput(format!("服务器报的文件大小 {length} 字节超出范围"))
                    })
                })
                .transpose()?,
            Some(file.expected.accepts_ranges),
            file.expected.etag.as_deref(),
            file.expected.last_modified.as_deref(),
            file.expected.content_type.as_deref(),
            file.chunk_size
                .map(|size| i64::try_from(size.get()).expect("每个请求的字节数远小于 i64 的上限")),
        ),
    };
    let (info_hash, root, trusted) = match &spec.kind {
        Kind::Hls(_) | Kind::File(_) => (None, None, None),
        Kind::Torrent(torrent) => {
            if let Some(existing) = tx
                .query_row(
                    "SELECT id FROM task WHERE torrent_info_hash = ?1",
                    [&torrent.info_hash],
                    |row| row.get(0),
                )
                .optional()?
            {
                return Err(Error::TorrentTaken(TaskId(existing)));
            }
            // 新建时的续传数据还没有已有的块
            (
                Some(torrent.info_hash.as_str()),
                Some(named_utf8(&torrent.root)?),
                Some(true),
            )
        }
    };
    let taken = taken_paths(tx)?;
    for path in [&mp4, &hls, &file_path, &root, &work_dir_text]
        .into_iter()
        .flatten()
    {
        if taken.contains(&naming::path_key(path)) {
            return Err(Error::PathTaken(path.into()));
        }
    }
    tx.execute(
        "INSERT INTO task (kind, title, url, headers, preference, output_kind, mp4_path, hls_path, live, file_path,
                           file_length, file_etag, file_last_modified, file_content_type, chunk_size, work_dir,
                           status, created_at, file_accepts_ranges, torrent_info_hash, torrent_root, torrent_resume,
                           torrent_trusted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, 'running', ?17, ?18, ?19, ?20,
                 ?21, ?22)",
        params![
            kind_code(&spec.kind),
            spec.title,
            spec.url.as_str(),
            to_json(&spec.headers),
            preference,
            output_kind,
            mp4,
            hls,
            live,
            file_path,
            length,
            etag,
            last_modified,
            content_type,
            chunk,
            work_dir_text,
            created_at,
            accepts_ranges,
            info_hash,
            root,
            resume,
            trusted,
        ],
    )?;
    Ok(TaskId(tx.last_insert_rowid()))
}

/// 全部任务，按新建先后。
pub fn list(tx: &Transaction<'_>) -> Result<Vec<Task>, Error> {
    let mut statement = tx.prepare(&format!("SELECT {COLUMNS} FROM task ORDER BY id"))?;
    let rows = statement.query_map([], Row::read)?;
    rows.map(|row| row?.into_task()).collect()
}

/// 一个任务；没有时为 None。
pub fn get(tx: &Transaction<'_>, id: TaskId) -> Result<Option<Task>, Error> {
    tx.query_row(
        &format!("SELECT {COLUMNS} FROM task WHERE id = ?1"),
        [id.0],
        Row::read,
    )
    .optional()?
    .map(Row::into_task)
    .transpose()
}

/// 已有任务占用的路径的比较键（[`naming::path_key`]）：各任务的 MP4、本地 HLS 目录、文件、BT 内容的根与任务目录。新建任务选主名时避开
/// 它们。
pub fn taken_paths(tx: &Transaction<'_>) -> Result<HashSet<String>, Error> {
    taken_except(tx, None)
}

/// 除任务 `except` 之外的任务占用的路径的比较键；`except` 为 None 时为全部任务的。
fn taken_except(tx: &Transaction<'_>, except: Option<TaskId>) -> Result<HashSet<String>, Error> {
    let mut statement = tx.prepare(
        "SELECT mp4_path, hls_path, file_path, torrent_root, work_dir FROM task WHERE ?1 IS NULL OR id != ?1",
    )?;
    let rows = statement.query_map([except.map(|id| id.0)], |row| {
        Ok([
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ])
    })?;
    let mut taken = HashSet::new();
    for row in rows {
        taken.extend(
            row?.into_iter()
                .flatten()
                .map(|path| naming::path_key(&path)),
        );
    }
    Ok(taken)
}

/// 换掉 HLS 任务的输出；新的输出路径与别的任务的输出或任务目录相同时报 [`Error::PathTaken`]，没有这个 HLS 任务时报
/// [`Error::NotFound`]。任务目录不变。
pub fn set_output(tx: &Transaction<'_>, id: TaskId, output: &OutputSpec) -> Result<(), Error> {
    let (kind, mp4, hls) = output_columns(output)?;
    let others = taken_except(tx, Some(id))?;
    for path in [mp4.as_deref(), hls.as_deref()].into_iter().flatten() {
        if others.contains(&naming::path_key(path)) {
            return Err(Error::PathTaken(path.into()));
        }
    }
    if tx.execute(
        "UPDATE task SET output_kind = ?2, mp4_path = ?3, hls_path = ?4 WHERE id = ?1 AND kind = 'hls'",
        params![id.0, kind, mp4, hls],
    )? == 0
    {
        return Err(Error::NotFound(id));
    }
    Ok(())
}

/// 换掉任务的请求头；没有这个任务时报 [`Error::NotFound`]。
pub fn set_headers(
    tx: &Transaction<'_>,
    id: TaskId,
    headers: &[(String, String)],
) -> Result<(), Error> {
    if tx.execute(
        "UPDATE task SET headers = ?2 WHERE id = ?1",
        params![id.0, to_json(&headers)],
    )? == 0
    {
        return Err(Error::NotFound(id));
    }
    Ok(())
}

/// 记下 HLS 点播的总片数；没有这个 HLS 任务时报 [`Error::NotFound`]。
pub fn set_total(tx: &Transaction<'_>, id: TaskId, total: usize) -> Result<(), Error> {
    let total = i64::try_from(total).expect("分片数远小于 i64 的上限");
    if tx.execute(
        "UPDATE task SET total = ?2 WHERE id = ?1 AND kind = 'hls'",
        params![id.0, total],
    )? == 0
    {
        return Err(Error::NotFound(id));
    }
    Ok(())
}

/// 记下完成时写出的结果；没有这个任务时报 [`Error::NotFound`]。
pub fn set_result(tx: &Transaction<'_>, id: TaskId, result: &Finished) -> Result<(), Error> {
    let millis = result
        .duration
        .map(|d| i64::try_from(d.as_millis()).expect("时长的毫秒数远小于 i64 的上限"));
    let bytes = i64::try_from(result.bytes).expect("字节数远小于 i64 的上限");
    let (width, height) = result.resolution.unzip();
    if tx.execute(
        "UPDATE task SET duration_ms = ?2, bytes = ?3, width = ?4, height = ?5 WHERE id = ?1",
        params![id.0, millis, bytes, width, height],
    )? == 0
    {
        return Err(Error::NotFound(id));
    }
    Ok(())
}

/// 记下继续时是否只写出已录的（见 [`Task::write_only`]）；没有这个任务时报 [`Error::NotFound`]。文件任务只能记为
/// false（表上有 CHECK）。
pub fn set_write_only(tx: &Transaction<'_>, id: TaskId, write_only: bool) -> Result<(), Error> {
    if tx.execute(
        "UPDATE task SET write_only = ?2 WHERE id = ?1",
        params![id.0, write_only],
    )? == 0
    {
        return Err(Error::NotFound(id));
    }
    Ok(())
}

/// 改状态；没有这个任务时报 [`Error::NotFound`]。
pub fn set_status(tx: &Transaction<'_>, id: TaskId, status: &Status) -> Result<(), Error> {
    let (name, error, failure) = match status {
        Status::Running => ("running", None, None),
        Status::Paused => ("paused", None, None),
        Status::Failed(failure) => (
            "failed",
            Some(failure.message.as_str()),
            Some(failure.kind.code()),
        ),
        Status::Done => ("done", None, None),
        Status::DiscardUnfinished => ("discard-unfinished", None, None),
    };
    let changed = tx.execute(
        "UPDATE task SET status = ?2, error = ?3, failure = ?4 WHERE id = ?1",
        params![id.0, name, error, failure],
    )?;
    if changed == 0 {
        return Err(Error::NotFound(id));
    }
    Ok(())
}

/// 把还标着运行中的任务改为暂停：应用上次异常退出时没来得及改；返回改了几个。其中 BT 任务的续传数据改为不可信：
/// 运行中下了的块没记进去，而存的续传数据可能比磁盘上的多（写盘与改文件在它之后）。
pub fn pause_interrupted(tx: &Transaction<'_>) -> Result<usize, Error> {
    tx.execute(
        "UPDATE task SET torrent_trusted = 0 WHERE status = 'running' AND kind = 'torrent'",
        [],
    )?;
    Ok(tx.execute(
        "UPDATE task SET status = 'paused' WHERE status = 'running'",
        [],
    )?)
}

/// BT 任务的续传数据；没有这个 BT 任务时报 [`Error::NotFound`]。
pub fn torrent_resume(tx: &Transaction<'_>, id: TaskId) -> Result<Resume, Error> {
    tx.query_row(
        "SELECT torrent_resume, torrent_trusted FROM task WHERE id = ?1 AND kind = 'torrent'",
        [id.0],
        |row| {
            Ok(Resume {
                data: row.get(0)?,
                trusted: row.get(1)?,
            })
        },
    )
    .optional()?
    .ok_or(Error::NotFound(id))
}

/// 换掉 BT 任务的续传数据；没有这个 BT 任务时报 [`Error::NotFound`]。
pub fn set_torrent_resume(tx: &Transaction<'_>, id: TaskId, resume: &Resume) -> Result<(), Error> {
    if tx.execute(
        "UPDATE task SET torrent_resume = ?2, torrent_trusted = ?3 WHERE id = ?1 AND kind = 'torrent'",
        params![id.0, resume.data, resume.trusted],
    )? == 0
    {
        return Err(Error::NotFound(id));
    }
    Ok(())
}

/// 上次关闭时存下的 BT 会话状态；没存过的为空（端口为 None、DHT 状态为空）。
pub fn torrent_session(tx: &Transaction<'_>) -> Result<SessionState, Error> {
    let (port, dht): (Option<u16>, Option<Vec<u8>>) = tx.query_row(
        "SELECT torrent_port, torrent_dht FROM settings WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(SessionState {
        port,
        dht: dht.unwrap_or_default(),
    })
}

/// 记下关闭时的 BT 会话状态。
pub fn set_torrent_session(tx: &Transaction<'_>, state: &SessionState) -> Result<(), Error> {
    tx.execute(
        "UPDATE settings SET torrent_port = ?1, torrent_dht = ?2 WHERE id = 1",
        params![state.port, state.dht],
    )?;
    Ok(())
}

/// 删除任务；没有时报 [`Error::NotFound`]。
pub fn delete(tx: &Transaction<'_>, id: TaskId) -> Result<(), Error> {
    if tx.execute("DELETE FROM task WHERE id = ?1", [id.0])? == 0 {
        return Err(Error::NotFound(id));
    }
    Ok(())
}

/// 表里的一行，原样读出、未校验。
struct Row {
    id: i64,
    kind: String,
    title: String,
    url: String,
    headers: String,
    preference: Option<String>,
    output_kind: Option<String>,
    mp4_path: Option<String>,
    hls_path: Option<String>,
    live: Option<bool>,
    file_path: Option<String>,
    file_length: Option<i64>,
    file_etag: Option<String>,
    file_last_modified: Option<String>,
    file_content_type: Option<String>,
    chunk_size: Option<i64>,
    work_dir: Option<String>,
    status: String,
    error: Option<String>,
    failure: Option<String>,
    created_at: i64,
    total: Option<i64>,
    duration_ms: Option<i64>,
    bytes: Option<i64>,
    width: Option<u32>,
    height: Option<u32>,
    write_only: bool,
    file_accepts_ranges: Option<bool>,
    torrent_info_hash: Option<String>,
    torrent_root: Option<String>,
}

impl Row {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
        Ok(Row {
            id: row.get(0)?,
            kind: row.get(1)?,
            title: row.get(2)?,
            url: row.get(3)?,
            headers: row.get(4)?,
            preference: row.get(5)?,
            output_kind: row.get(6)?,
            mp4_path: row.get(7)?,
            hls_path: row.get(8)?,
            live: row.get(9)?,
            file_path: row.get(10)?,
            file_length: row.get(11)?,
            file_etag: row.get(12)?,
            file_last_modified: row.get(13)?,
            file_content_type: row.get(14)?,
            chunk_size: row.get(15)?,
            work_dir: row.get(16)?,
            status: row.get(17)?,
            error: row.get(18)?,
            failure: row.get(19)?,
            created_at: row.get(20)?,
            total: row.get(21)?,
            duration_ms: row.get(22)?,
            bytes: row.get(23)?,
            width: row.get(24)?,
            height: row.get(25)?,
            write_only: row.get(26)?,
            file_accepts_ranges: row.get(27)?,
            torrent_info_hash: row.get(28)?,
            torrent_root: row.get(29)?,
        })
    }

    fn into_task(self) -> Result<Task, Error> {
        let id = TaskId(self.id);
        let corrupt = |what: &str| Error::Corrupt(format!("任务 {} 的{what}", self.id));
        let url = self.url.parse().map_err(|_| corrupt("地址"))?;
        let headers: Vec<(String, String)> =
            serde_json::from_str(&self.headers).map_err(|_| corrupt("请求头"))?;
        let kind = match (
            self.kind.as_str(),
            self.preference,
            self.output_kind,
            self.live,
            self.file_path,
            self.file_accepts_ranges,
            self.torrent_info_hash,
            self.torrent_root,
        ) {
            ("hls", Some(preference), Some(output_kind), Some(live), None, None, None, None) => {
                let preference: StoredPreference =
                    serde_json::from_str(&preference).map_err(|_| corrupt("选轨偏好"))?;
                let output = output_spec(&output_kind, self.mp4_path, self.hls_path)
                    .ok_or_else(|| corrupt("输出"))?;
                Kind::Hls(HlsSpec {
                    preference,
                    output,
                    live,
                })
            }
            ("file", None, None, None, Some(path), Some(accepts_ranges), None, None) => {
                let path = PathBuf::from(path);
                if !named_absolute(&path) {
                    return Err(corrupt("输出"));
                }
                let length = self
                    .file_length
                    .map(u64::try_from)
                    .transpose()
                    .map_err(|_| corrupt("文件的字节数"))?;
                let chunk_size = self
                    .chunk_size
                    .map(|size| u64::try_from(size).ok().and_then(std::num::NonZeroU64::new))
                    .map(|size| size.ok_or_else(|| corrupt("每个请求的字节数")))
                    .transpose()?;
                Kind::File(FileSpec {
                    path,
                    expected: Expected {
                        length,
                        accepts_ranges,
                        etag: self.file_etag,
                        last_modified: self.file_last_modified,
                        content_type: self.file_content_type,
                    },
                    chunk_size,
                })
            }
            ("torrent", None, None, None, None, None, Some(info_hash), Some(root)) => {
                let root = PathBuf::from(root);
                if !named_absolute(&root) {
                    return Err(corrupt("内容的根"));
                }
                Kind::Torrent(TorrentSpec { info_hash, root })
            }
            _ => return Err(corrupt("种类")),
        };
        let work_dir = match self.work_dir.map(PathBuf::from) {
            Some(dir) if !named_absolute(&dir) => return Err(corrupt("任务目录")),
            dir => dir,
        };
        let total = self
            .total
            .map(usize::try_from)
            .transpose()
            .map_err(|_| corrupt("总片数"))?;
        let result = match (self.bytes, self.width, self.height) {
            (None, None, None) if self.duration_ms.is_none() => None,
            (Some(bytes), width, height) if width.is_some() == height.is_some() => Some(Finished {
                duration: self
                    .duration_ms
                    .map(|ms| u64::try_from(ms).map(Duration::from_millis))
                    .transpose()
                    .map_err(|_| corrupt("时长"))?,
                bytes: u64::try_from(bytes).map_err(|_| corrupt("字节数"))?,
                resolution: width.zip(height),
            }),
            _ => return Err(corrupt("写出的结果")),
        };
        let status = match (self.status.as_str(), self.error, self.failure) {
            ("running", None, None) => Status::Running,
            ("paused", None, None) => Status::Paused,
            ("failed", Some(message), Some(code)) => Status::Failed(Failure {
                kind: FailureKind::parse(&code).ok_or_else(|| corrupt("失败的种类"))?,
                message,
            }),
            ("done", None, None) => Status::Done,
            ("discard-unfinished", None, None) => Status::DiscardUnfinished,
            _ => return Err(corrupt("状态")),
        };
        Ok(Task {
            id,
            spec: TaskSpec {
                title: self.title,
                url,
                headers,
                kind,
            },
            status,
            work_dir,
            created_at: self.created_at,
            total,
            result,
            write_only: self.write_only,
        })
    }
}

/// 表里的输出三列读回成输出；种类与路径对不上、或路径不是以文件名结尾的绝对路径时为 None。
fn output_spec(kind: &str, mp4: Option<String>, hls: Option<String>) -> Option<OutputSpec> {
    let output = match (kind, mp4, hls) {
        ("mp4", Some(mp4), None) => OutputSpec::Mp4(mp4.into()),
        ("hls", None, Some(hls)) => OutputSpec::Hls(hls.into()),
        ("both", Some(mp4), Some(hls)) => OutputSpec::Both {
            mp4: mp4.into(),
            hls: hls.into(),
        },
        _ => return None,
    };
    let valid = match &output {
        OutputSpec::Mp4(path) | OutputSpec::Hls(path) => named_absolute(path),
        OutputSpec::Both { mp4, hls } => named_absolute(mp4) && named_absolute(hls),
    };
    valid.then_some(output)
}

/// 是以文件名结尾的绝对路径（不以 `..` 或根结尾）。
fn named_absolute(path: &Path) -> bool {
    path.is_absolute() && path.file_name().is_some()
}

/// 种类在表里的写法（表上有 CHECK）。
fn kind_code(kind: &Kind) -> &'static str {
    match kind {
        Kind::Hls(_) => "hls",
        Kind::File(_) => "file",
        Kind::Torrent(_) => "torrent",
    }
}

/// 输出路径在表里的写法：须是以文件名结尾的绝对路径（读回时同样核对），且是 UTF-8。
fn named_utf8(path: &Path) -> Result<String, Error> {
    if !named_absolute(path) {
        return Err(Error::InvalidInput(format!(
            "输出不是以文件名结尾的绝对路径：{}",
            path.display()
        )));
    }
    utf8(path)
}

/// 第 5 版：设置加外观主题，写法见 [`theme_code`]，没设过为日蚀。
fn add_theme(tx: &Transaction<'_>) -> Result<(), Error> {
    tx.execute_batch(
        "ALTER TABLE settings ADD COLUMN
            theme TEXT NOT NULL DEFAULT 'eclipse' CHECK (theme IN ('eclipse', 'blood-moon'));",
    )?;
    Ok(())
}

/// 路径在表里的写法：须是 UTF-8（核心对输出路径的要求相同）。
fn utf8(path: &Path) -> Result<String, Error> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidInput(format!("路径不是 UTF-8：{}", path.display())))
}

/// 读出设置；保存目录不是绝对路径、代理或主题认不出时报 [`Error::Corrupt`]。
pub fn settings(tx: &Transaction<'_>) -> Result<Settings, Error> {
    let (folder, proxy, theme): (Option<String>, String, String) = tx.query_row(
        "SELECT folder, proxy, theme FROM settings WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let folder = folder
        .map(PathBuf::from)
        .map(|folder| {
            folder
                .is_absolute()
                .then_some(folder)
                .ok_or_else(|| Error::Corrupt("设置里的保存目录".to_owned()))
        })
        .transpose()?;
    Ok(Settings {
        folder,
        proxy: parse_proxy(&proxy)?,
        theme: parse_theme(&theme)?,
    })
}

/// 记下新建任务默认保存到的目录 `folder`（绝对路径，调用方已核对）；不是 UTF-8 时报 [`Error::InvalidInput`]。
pub fn set_folder(tx: &Transaction<'_>, folder: &Path) -> Result<(), Error> {
    tx.execute(
        "UPDATE settings SET folder = ?1 WHERE id = 1",
        params![utf8(folder)?],
    )?;
    Ok(())
}

/// 记下代理（调用方已核对能用）。
pub fn set_proxy(tx: &Transaction<'_>, proxy: &Proxy) -> Result<(), Error> {
    tx.execute(
        "UPDATE settings SET proxy = ?1 WHERE id = 1",
        params![proxy_code(proxy)],
    )?;
    Ok(())
}

/// 记下外观主题。
pub fn set_theme(tx: &Transaction<'_>, theme: Theme) -> Result<(), Error> {
    tx.execute(
        "UPDATE settings SET theme = ?1 WHERE id = 1",
        params![theme_code(theme)],
    )?;
    Ok(())
}

/// 主题在存储里的写法：`eclipse`、`blood-moon`。存储格式的一部分，表上有 CHECK。
fn theme_code(theme: Theme) -> &'static str {
    match theme {
        Theme::Eclipse => "eclipse",
        Theme::BloodMoon => "blood-moon",
    }
}

/// 读回 [`theme_code`] 的写法；认不出时报 [`Error::Corrupt`]。
fn parse_theme(code: &str) -> Result<Theme, Error> {
    match code {
        "eclipse" => Ok(Theme::Eclipse),
        "blood-moon" => Ok(Theme::BloodMoon),
        _ => Err(Error::Corrupt("设置里的外观主题".to_owned())),
    }
}

/// 代理在存储里的写法：`system`、`direct`，或 `url:<代理地址>`（明文，可带口令，与请求头一样只有本人可读写）。
/// 存储格式的一部分，表上有 CHECK。
fn proxy_code(proxy: &Proxy) -> String {
    match proxy {
        Proxy::System => "system".to_owned(),
        Proxy::Direct => "direct".to_owned(),
        Proxy::Url(url) => format!("url:{url}"),
    }
}

/// 读回 [`proxy_code`] 的写法；认不出时报 [`Error::Corrupt`]。
fn parse_proxy(code: &str) -> Result<Proxy, Error> {
    match code {
        "system" => Ok(Proxy::System),
        "direct" => Ok(Proxy::Direct),
        _ => code
            .strip_prefix("url:")
            .and_then(|url| Url::parse(url).ok())
            .map(Proxy::Url)
            .ok_or_else(|| Error::Corrupt("设置里的代理".to_owned())),
    }
}

/// 输出在表里的三列：种类与两个路径。
fn output_columns(
    output: &OutputSpec,
) -> Result<(&'static str, Option<String>, Option<String>), Error> {
    Ok(match output {
        OutputSpec::Mp4(mp4) => ("mp4", Some(utf8(mp4)?), None),
        OutputSpec::Hls(hls) => ("hls", None, Some(utf8(hls)?)),
        OutputSpec::Both { mp4, hls } => ("both", Some(utf8(mp4)?), Some(utf8(hls)?)),
    })
}

/// 表结构版本在 `user_version` 里的写法。
fn schema_version(version: usize) -> i64 {
    i64::try_from(version).expect("迁移的项数远小于 i64 的上限")
}

fn to_json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).expect("只含字符串、整数、数组与枚举，序列化不会失败")
}

fn io(action: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.to_path_buf();
    move |cause| Error::Io {
        action,
        path,
        cause,
    }
}

/// 存储文件只有本人可读写。Windows 上用户目录本就只有本人能访问，不另设。
fn restrict_to_owner(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(io("设置权限", path))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use hs_m3u8_core::hls::Preference;

    use super::*;

    #[test]
    fn proxies_read_back_as_written() {
        for proxy in [
            Proxy::System,
            Proxy::Direct,
            Proxy::Url(Url::parse("socks5h://user:pass@127.0.0.1:1080").unwrap()),
        ] {
            assert!(parse_proxy(&proxy_code(&proxy)).unwrap() == proxy);
        }
        assert!(matches!(parse_proxy("pac:x"), Err(Error::Corrupt(_))));
    }

    /// 测试用的绝对路径 `/m/<name>`；Windows 上没有盘符的不算绝对路径，为 `C:\m\<name>`。只用作存储里的值，不访问
    /// 文件系统。
    fn m(name: &str) -> PathBuf {
        let root = if cfg!(windows) { r"C:\m" } else { "/m" };
        Path::new(root).join(name)
    }

    /// [`m`] 在表里的写法。
    fn text(name: &str) -> String {
        utf8(&m(name)).unwrap()
    }

    fn spec(output: OutputSpec) -> TaskSpec {
        TaskSpec {
            title: "主题演讲".into(),
            url: "https://media.example.com/keynote/master.m3u8?token=1"
                .parse()
                .unwrap(),
            headers: vec![("Referer".into(), "https://example.com/".into())],
            kind: Kind::Hls(HlsSpec {
                preference: StoredPreference::try_from(&Preference::default()).unwrap(),
                output,
                live: true,
            }),
        }
    }

    fn file_spec(path: PathBuf) -> TaskSpec {
        TaskSpec {
            title: "数据.zip".into(),
            url: "https://files.example.com/d/1".parse().unwrap(),
            headers: Vec::new(),
            kind: Kind::File(FileSpec {
                path,
                expected: Expected {
                    length: Some(1 << 40),
                    accepts_ranges: true,
                    etag: Some("\"v1\"".into()),
                    last_modified: None,
                    content_type: Some("application/zip".into()),
                },
                chunk_size: std::num::NonZeroU64::new(10 << 20),
            }),
        }
    }

    /// 文件任务读回写入的内容；它的输出与任务目录同样算占用。
    #[test]
    fn file_tasks_round_trip() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let spec = file_spec(m("数据.zip"));
        let id = insert(&tx, &spec, Some(&m("数据.hsdl")), None, 1).unwrap();
        let task = get(&tx, id).unwrap().unwrap();
        assert_eq!(
            (task.spec, task.total, task.write_only),
            (spec, None, false)
        );
        let taken = taken_paths(&tx).unwrap();
        for name in ["数据.zip", "数据.hsdl"] {
            assert!(taken.contains(&naming::path_key(&text(name))), "{name}");
        }
        assert!(matches!(
            insert(&tx, &file_spec(m("数据.ZIP")), Some(&m("x.hsdl")), None, 2),
            Err(Error::PathTaken(_))
        ));
        // 只属于 HLS 的不记到文件任务上
        assert!(matches!(set_total(&tx, id, 3), Err(Error::NotFound(_))));
        assert!(set_write_only(&tx, id, true).is_err());
        let output = OutputSpec::Mp4(m("y.mp4"));
        assert!(matches!(
            set_output(&tx, id, &output),
            Err(Error::NotFound(_))
        ));
    }

    /// 种类与各列对不上的行由表拦下；对得上的两行能插入（说明拦下的正是多出或缺少的那一列）。
    #[test]
    fn kinds_and_columns_must_agree() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        // 种类、选轨偏好、输出种类、是否录直播、文件路径、是否支持范围请求
        let hls = "'hls', '{}', 'mp4', 0, NULL, NULL";
        let file = "'file', NULL, NULL, NULL, '/m/a', 1";
        let insert = |kind_columns: &str, extra_columns: &str, extra_values: &str| {
            tx.execute(
                &format!(
                    "INSERT INTO task (kind, preference, output_kind, live, file_path, file_accepts_ranges, title,
                                       url, headers, status, created_at{extra_columns})
                     VALUES ({kind_columns}, 'x', 'https://a/b', '[]', 'paused', 1{extra_values})"
                ),
                [],
            )
        };
        insert(hls, "", "").unwrap();
        insert(file, "", "").unwrap();
        for (kind_columns, extra_columns, extra_values) in [
            // HLS 缺选轨偏好；文件缺路径、缺是否支持范围请求
            ("'hls', NULL, 'mp4', 0, NULL, NULL", "", ""),
            ("'file', NULL, NULL, NULL, NULL, 1", "", ""),
            ("'file', NULL, NULL, NULL, '/m/a', NULL", "", ""),
            // 文件带了 HLS 的列
            ("'file', NULL, NULL, 0, '/m/a', 1", "", ""),
            (file, ", total", ", 3"),
            // HLS 带了文件的列
            ("'hls', '{}', 'mp4', 0, '/m/a', NULL", "", ""),
            ("'hls', '{}', 'mp4', 0, NULL, 0", "", ""),
            (hls, ", chunk_size", ", 1"),
            (hls, ", file_etag", ", 'e'"),
        ] {
            assert!(
                insert(kind_columns, extra_columns, extra_values).is_err(),
                "{kind_columns}{extra_columns}"
            );
        }
    }

    /// 第 3 版的存储升上来时编号计数照旧：删掉的最大编号不再分配。
    #[test]
    fn rebuilding_for_kinds_keeps_the_id_counter() {
        let dir = scratch("v3-counter");
        let path = dir.join("tasks.sqlite");
        {
            let mut conn = Connection::open(&path).unwrap();
            let tx = conn.transaction().unwrap();
            for migration in &MIGRATIONS[..3] {
                migration(&tx).unwrap();
            }
            for title in ["甲", "乙"] {
                tx.execute(
                    "INSERT INTO task (title, url, preference, headers, output_kind, mp4_path, live, status, created_at)
                     VALUES (?1, 'https://a/b.m3u8', '{}', '[]', 'mp4', ?2, 0, 'paused', 1)",
                    params![title, text(&format!("{title}.mp4"))],
                )
                .unwrap();
            }
            tx.execute("DELETE FROM task WHERE title = '乙'", [])
                .unwrap();
            tx.pragma_update(None, "user_version", 3).unwrap();
            tx.commit().unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        let tx = store.transaction().unwrap();
        let next = add(&tx, OutputSpec::Mp4(m("丙.mp4")), 2).unwrap();
        assert_eq!(next.0, 3);
        drop(tx);
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 第 6 版的 HLS 与文件任务升到第 7 版后原样读回，编号计数照旧。
    #[test]
    fn rebuilding_for_torrents_keeps_rows_and_the_id_counter() {
        let dir = scratch("v6-rows");
        let path = dir.join("tasks.sqlite");
        let (hls, file) = (spec(OutputSpec::Mp4(m("甲.mp4"))), file_spec(m("数据.zip")));
        {
            let mut conn = Connection::open(&path).unwrap();
            let tx = conn.transaction().unwrap();
            for migration in &MIGRATIONS[..6] {
                migration(&tx).unwrap();
            }
            tx.execute(
                "INSERT INTO task (kind, title, url, headers, preference, output_kind, mp4_path, live, work_dir,
                                   status, failure, error, created_at, total)
                 VALUES ('hls', ?1, ?2, ?3, ?4, 'mp4', ?5, 1, ?6, 'failed', 'status:403', 'HTTP 403', 7, 12)",
                params![
                    hls.title,
                    hls.url.as_str(),
                    to_json(&hls.headers),
                    to_json(&hls.hls().unwrap().preference),
                    text("甲.mp4"),
                    text("甲.hsdl"),
                ],
            )
            .unwrap();
            let expected = &file.file().unwrap().expected;
            tx.execute(
                "INSERT INTO task (kind, title, url, headers, file_path, file_length, file_accepts_ranges, file_etag,
                                   file_content_type, chunk_size, work_dir, status, created_at, bytes)
                 VALUES ('file', ?1, ?2, '[]', ?3, ?4, 1, ?5, ?6, ?7, ?8, 'done', 8, 5)",
                params![
                    file.title,
                    file.url.as_str(),
                    text("数据.zip"),
                    i64::try_from(expected.length.unwrap()).unwrap(),
                    expected.etag,
                    expected.content_type,
                    10 << 20,
                    text("数据.zip.hsdl"),
                ],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO task (kind, title, url, headers, file_path, file_accepts_ranges, status, created_at)
                 VALUES ('file', 'x', 'https://a/x', '[]', ?1, 0, 'paused', 9)",
                params![text("x")],
            )
            .unwrap();
            tx.execute("DELETE FROM task WHERE title = 'x'", [])
                .unwrap();
            tx.pragma_update(None, "user_version", 6).unwrap();
            tx.commit().unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        let tx = store.transaction().unwrap();
        let tasks = list(&tx).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!((&tasks[0].spec, tasks[0].total), (&hls, Some(12)));
        assert_eq!(
            tasks[0].status,
            Status::Failed(Failure {
                kind: FailureKind::Status(403),
                message: "HTTP 403".into()
            })
        );
        assert_eq!(
            (&tasks[1].spec, tasks[1].result.as_ref().map(|r| r.bytes)),
            (&file, Some(5))
        );
        assert_eq!(
            tasks[1].work_dir.as_deref(),
            Some(m("数据.zip.hsdl").as_path())
        );
        let next = add(&tx, OutputSpec::Mp4(m("乙.mp4")), 10).unwrap();
        assert_eq!(next.0, 4);
        drop(tx);
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 新建任务，任务目录取核心的默认位置。
    fn add(tx: &Transaction<'_>, output: OutputSpec, created_at: i64) -> Result<TaskId, Error> {
        let work_dir = output.options().resolved_work_dir().unwrap();
        insert(tx, &spec(output), Some(&work_dir), None, created_at)
    }

    #[test]
    fn tasks_round_trip_and_change_status() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let a = add(&tx, OutputSpec::Mp4(m("a.mp4")), 100).unwrap();
        let both = OutputSpec::Both {
            mp4: m("b.mp4"),
            hls: m("b"),
        };
        let b = add(&tx, both, 200).unwrap();
        let forbidden = Failure {
            kind: FailureKind::Status(403),
            message: "HTTP 状态 403".into(),
        };
        set_status(&tx, a, &Status::Failed(forbidden.clone())).unwrap();
        set_status(&tx, b, &Status::Done).unwrap();
        tx.commit().unwrap();

        let tx = store.transaction().unwrap();
        let tasks = list(&tx).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].spec, spec(OutputSpec::Mp4(m("a.mp4"))));
        assert_eq!(tasks[0].work_dir.as_deref(), Some(m("a.hsdl").as_path()));
        assert_eq!(tasks[0].status, Status::Failed(forbidden));
        assert_eq!(tasks[1].status, Status::Done);
        assert_eq!(get(&tx, b).unwrap().unwrap().created_at, 200);
        delete(&tx, a).unwrap();
        assert!(matches!(delete(&tx, a), Err(Error::NotFound(_))));
        assert!(matches!(
            set_status(&tx, a, &Status::Done),
            Err(Error::NotFound(_))
        ));
        assert_eq!(get(&tx, a).unwrap(), None);
    }

    /// 每一种失败都写得进存储、读得回来（存储里的 CHECK 列着全部写法）。
    #[test]
    fn every_failure_kind_is_stored() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let id = add(&tx, OutputSpec::Mp4(m("a.mp4")), 1).unwrap();
        for kind in crate::failure::every() {
            let failed = Status::Failed(Failure {
                kind,
                message: "x".into(),
            });
            set_status(&tx, id, &failed).unwrap();
            assert_eq!(get(&tx, id).unwrap().unwrap().status, failed);
        }
    }

    #[test]
    fn interrupted_tasks_become_paused() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let running = add(&tx, OutputSpec::Hls(m("c")), 1).unwrap();
        let done = add(&tx, OutputSpec::Hls(m("d")), 2).unwrap();
        set_status(&tx, done, &Status::Done).unwrap();
        assert_eq!(pause_interrupted(&tx).unwrap(), 1);
        assert_eq!(get(&tx, running).unwrap().unwrap().status, Status::Paused);
        assert_eq!(get(&tx, done).unwrap().unwrap().status, Status::Done);
    }

    fn torrent_spec(info_hash: &str, root: PathBuf) -> TaskSpec {
        TaskSpec {
            title: "专辑".into(),
            url: format!("magnet:?xt=urn:btih:{info_hash}").parse().unwrap(),
            headers: Vec::new(),
            kind: Kind::Torrent(TorrentSpec {
                info_hash: info_hash.into(),
                root,
            }),
        }
    }

    /// BT 任务读回写入的内容，续传数据另读另写；内容的根算占用，同一个种子不能有两个任务；异常退出时还在运行的，续传
    /// 数据改为不可信。
    #[test]
    fn torrent_tasks_round_trip() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let spec = torrent_spec(hash, m("专辑"));
        let id = insert(&tx, &spec, None, Some(b"resume"), 1).unwrap();
        let task = get(&tx, id).unwrap().unwrap();
        assert_eq!((task.spec, task.work_dir), (spec, None));
        assert_eq!(
            torrent_resume(&tx, id).unwrap(),
            Resume {
                data: b"resume".to_vec(),
                trusted: true
            }
        );
        assert!(
            taken_paths(&tx)
                .unwrap()
                .contains(&naming::path_key(&text("专辑")))
        );
        assert!(matches!(
            insert(&tx, &torrent_spec(hash, m("别处")), None, Some(b"r"), 2),
            Err(Error::TorrentTaken(at)) if at == id
        ));
        let other = "1123456789abcdef0123456789abcdef01234567";
        assert!(matches!(
            insert(&tx, &torrent_spec(other, m("专辑")), None, Some(b"r"), 2),
            Err(Error::PathTaken(_))
        ));
        let paused = Resume {
            data: b"paused".to_vec(),
            trusted: true,
        };
        set_torrent_resume(&tx, id, &paused).unwrap();
        assert_eq!(torrent_resume(&tx, id).unwrap(), paused);
        assert_eq!(pause_interrupted(&tx).unwrap(), 1);
        assert_eq!(get(&tx, id).unwrap().unwrap().status, Status::Paused);
        assert!(!torrent_resume(&tx, id).unwrap().trusted);
        // 别的任务没有续传数据
        let hls = add(&tx, OutputSpec::Mp4(m("a.mp4")), 3).unwrap();
        assert!(matches!(torrent_resume(&tx, hls), Err(Error::NotFound(_))));
        assert!(matches!(
            set_torrent_resume(&tx, hls, &paused),
            Err(Error::NotFound(_))
        ));
    }

    /// BT 的四列必有且只用于它；它没有任务目录与请求头；编号是 40 个小写十六进制数字。
    #[test]
    fn torrent_columns_must_agree() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let hash = "'0123456789abcdef0123456789abcdef01234567'";
        let insert = |kind: &str, columns: &str, values: &str| {
            tx.execute(
                &format!(
                    "INSERT INTO task (kind, title, url, headers, status, created_at{columns})
                     VALUES ('{kind}', 'x', 'magnet:?', '[]', 'paused', 1{values})"
                ),
                [],
            )
        };
        let all = ", torrent_info_hash, torrent_root, torrent_resume, torrent_trusted";
        insert("torrent", all, &format!(", {hash}, '/m/a', x'00', 1")).unwrap();
        tx.execute("DELETE FROM task", []).unwrap();
        for (kind, columns, values) in [
            ("torrent", all, ", '0123', '/m/a', x'00', 1".to_owned()),
            (
                "torrent",
                all,
                ", '0123456789ABCDEF0123456789ABCDEF01234567', '/m/a', x'00', 1".to_owned(),
            ),
            (
                "torrent",
                ", torrent_info_hash, torrent_root, torrent_trusted",
                format!(", {hash}, '/m/a', 1"),
            ),
            (
                "torrent",
                &format!("{all}, work_dir"),
                format!(", {hash}, '/m/a', x'00', 1, '/m/a.hsdl'"),
            ),
            (
                "hls",
                ", preference, output_kind, live, torrent_root",
                ", '{}', 'mp4', 0, '/m/a'".to_owned(),
            ),
        ] {
            assert!(insert(kind, columns, &values).is_err(), "{kind}{columns}");
        }
    }

    /// 删掉编号最大的任务后，新任务也不拿它的编号。
    #[test]
    fn ids_are_not_reused() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let first = add(&tx, OutputSpec::Mp4(m("a.mp4")), 1).unwrap();
        delete(&tx, first).unwrap();
        let second = add(&tx, OutputSpec::Mp4(m("b.mp4")), 2).unwrap();
        assert!(second.0 > first.0, "{first:?} {second:?}");
    }

    /// 改过的存储里读不回的行报 [`Error::Corrupt`]：选轨偏好不是 JSON、输出是相对路径。
    #[test]
    fn rows_that_cannot_be_read_back_are_reported() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let preference = to_json(&StoredPreference::try_from(&Preference::default()).unwrap());
        for (preference, mp4) in [
            ("{}", text("x.mp4")),
            (preference.as_str(), "m/x.mp4".into()),
        ] {
            tx.execute(
                "INSERT INTO task (kind, title, url, preference, headers, output_kind, mp4_path, live, status,
                                   created_at)
                 VALUES ('hls', 'x', 'https://a/b.m3u8', ?1, '[]', 'mp4', ?2, 0, 'done', 1)",
                params![preference, mp4],
            )
            .unwrap();
            assert!(matches!(list(&tx), Err(Error::Corrupt(_))));
            tx.execute("DELETE FROM task", []).unwrap();
        }
    }

    /// 测试用的空目录，名字带进程号。
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hs-dlx-app-{name}-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_store_from_a_newer_version_is_refused() {
        let dir = scratch("newer");
        let path = dir.join("tasks.sqlite");
        drop(Store::open(&path).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", schema_version(MIGRATIONS.len() + 1))
            .unwrap();
        assert!(matches!(Store::open(&path), Err(Error::Corrupt(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 第 1 版的存储：建表、插入 `rows`（标题、状态、MP4 路径），版本记为 1。
    fn version_1(path: &Path, rows: &[(&str, &str, &str)]) {
        let mut conn = Connection::open(path).unwrap();
        let tx = conn.transaction().unwrap();
        create_task_table(&tx).unwrap();
        for (title, status, mp4) in rows {
            tx.execute(
                "INSERT INTO task (title, url, preference, headers, output_kind, mp4_path, live, status, error, created_at)
                 VALUES (?1, 'https://a/b.m3u8', ?2, '[]', 'mp4', ?3, 0, ?4, ?5, 1)",
                params![
                    title,
                    to_json(&StoredPreference::try_from(&Preference::default()).unwrap()),
                    mp4,
                    status,
                    (*status == "failed").then_some("403")
                ],
            )
            .unwrap();
        }
        tx.pragma_update(None, "user_version", 1).unwrap();
        tx.commit().unwrap();
    }

    /// 第 1 版的存储升到当前版本：没完成的任务补上按输出算出的任务目录（之后新建的任务避开它），已完成的没有；已完成的
    /// 与没完成的同名不算冲突。没完成的两个任务的任务目录只差大小写时，编号小的留着它，另一个改为失败、没有任务目录。
    /// 迁移后的编号接着原有的最大编号分配，也不重用。
    #[test]
    fn version_1_rows_get_their_work_dir() {
        let dir = scratch("migrate");
        let path = dir.join("tasks.sqlite");
        version_1(
            &path,
            &[
                ("甲", "paused", &text("a.mp4")),
                ("乙", "done", &text("A.mp4")),
                ("丙", "failed", &text("A.mp4")),
                ("丁", "running", &text("d.mp4")),
            ],
        );
        let mut store = Store::open(&path).unwrap();
        let tx = store.transaction().unwrap();
        let taken = taken_paths(&tx).unwrap();
        assert!(
            taken.contains(&naming::path_key(&text("a.hsdl"))),
            "{taken:?}"
        );
        let tasks = list(&tx).unwrap();
        let work_dirs: Vec<Option<&Path>> =
            tasks.iter().map(|task| task.work_dir.as_deref()).collect();
        assert_eq!(
            work_dirs,
            [
                Some(m("a.hsdl").as_path()),
                None,
                None,
                Some(m("d.hsdl").as_path())
            ]
        );
        assert_eq!(tasks[1].status, Status::Done);
        assert!(
            matches!(&tasks[2].status, Status::Failed(f) if f.message.contains("「甲」")),
            "{:?}",
            tasks[2].status
        );
        assert_eq!((tasks[0].total, &tasks[1].result), (None, &None));
        let last = tasks[3].id;
        delete(&tx, last).unwrap();
        let next = add(&tx, OutputSpec::Mp4(m("e.mp4")), 2).unwrap();
        assert_eq!(next.0, last.0 + 1);
        // 迁移来的存储有一份没设过的设置
        assert!(settings(&tx).unwrap() == Settings::default());
        drop(tx);
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 设置读回写入的值；不是本应用写的代理写法由表拦下。
    #[test]
    fn settings_round_trip() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        assert!(settings(&tx).unwrap() == Settings::default());
        set_folder(&tx, &m("影片")).unwrap();
        let proxy = Proxy::Url(hs_m3u8_core::Url::parse("http://127.0.0.1:7890").unwrap());
        set_proxy(&tx, &proxy).unwrap();
        set_theme(&tx, Theme::BloodMoon).unwrap();
        let read = settings(&tx).unwrap();
        assert_eq!(read.folder.as_deref(), Some(m("影片").as_path()));
        assert!(read.proxy == proxy);
        assert_eq!(read.theme, Theme::BloodMoon);
        assert!(
            tx.execute("UPDATE settings SET proxy = 'pac' WHERE id = 1", [])
                .is_err()
        );
        assert!(
            tx.execute("UPDATE settings SET theme = 'aurora' WHERE id = 1", [])
                .is_err()
        );
        let empty = torrent_session(&tx).unwrap();
        assert_eq!((empty.port, empty.dht.is_empty()), (None, true));
        set_torrent_session(
            &tx,
            &SessionState {
                port: Some(51413),
                dht: b"dht".to_vec(),
            },
        )
        .unwrap();
        let saved = torrent_session(&tx).unwrap();
        assert_eq!((saved.port, saved.dht), (Some(51413), b"dht".to_vec()));
    }

    /// 同一份存储同时只能被一个进程（这里是同一进程里的第二个连接）打开；先打开的释放后才能再开。
    #[test]
    fn a_store_is_opened_by_one_app_at_a_time() {
        let dir = scratch("in-use");
        let path = dir.join("tasks.sqlite");
        let first = Store::open(&path).unwrap();
        assert!(matches!(Store::open(&path), Err(Error::InUse(_))));
        drop(first);
        Store::open(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 任务目录与已有任务相同的拒收：MP4 `a.mp4` 与本地 HLS `A` 的任务目录都是 `a.hsdl`（只差大小写）。记下总片数后
    /// 读得回。
    #[test]
    fn work_dirs_are_unique_and_totals_are_kept() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let a = add(&tx, OutputSpec::Mp4(m("a.mp4")), 1).unwrap();
        assert!(matches!(
            add(&tx, OutputSpec::Hls(m("A")), 2),
            Err(Error::PathTaken(path)) if path == m("A.hsdl")
        ));
        set_total(&tx, a, 664).unwrap();
        assert_eq!(get(&tx, a).unwrap().unwrap().total, Some(664));
        assert!(matches!(
            set_total(&tx, TaskId(99), 1),
            Err(Error::NotFound(_))
        ));
    }
}
