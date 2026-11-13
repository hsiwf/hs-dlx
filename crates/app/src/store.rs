//! 任务存储：一个 SQLite 文件。表结构的版本记在 `PRAGMA user_version`，打开时按版本迁移；读写函数接受调用方
//! 开好的事务，事务边界由服务层定。

use std::collections::HashSet;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::preference::StoredPreference;
use crate::{Error, Finished, OutputSpec, Status, Task, TaskId, TaskSpec, naming};

/// 一次迁移：在给定的事务里把表结构升一个版本。
type Migration = fn(&Transaction<'_>) -> Result<(), Error>;

/// 各版本的迁移：第 i 项把版本 i 升到 i + 1。已发布的迁移不再改动，新的表结构只追加一项。
const MIGRATIONS: [Migration; 2] = [create_task_table, add_work_dir_total_and_result];

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

/// 第 2 版：
/// - 任务目录单列一列，另记它的比较键（[`naming::path_key`]）并且键唯一：两个任务共用一个任务目录时，放弃其中一个会
///   删掉另一个已下的内容。没完成的行按第 1 版的规则补算（与输出同级的 `<主名>.hsdl`），已完成的不占（任务目录已在
///   完成时删掉）；补算出重复的报 [`Error::Corrupt`]，说明是哪几个任务。
/// - 点播的总片数，与完成时写出的结果（时长、字节数、视频的宽高）。
fn add_work_dir_total_and_result(tx: &Transaction<'_>) -> Result<(), Error> {
    tx.execute_batch(
        "ALTER TABLE task ADD COLUMN work_dir TEXT;
         ALTER TABLE task ADD COLUMN work_dir_key TEXT;
         ALTER TABLE task ADD COLUMN total INTEGER CHECK (total >= 0);
         ALTER TABLE task ADD COLUMN duration_ms INTEGER CHECK (duration_ms >= 0);
         ALTER TABLE task ADD COLUMN bytes INTEGER CHECK (bytes >= 0);
         ALTER TABLE task ADD COLUMN width INTEGER CHECK (width > 0);
         ALTER TABLE task ADD COLUMN height INTEGER CHECK (height > 0);",
    )?;
    /// 第 1 版的一行里迁移要用的列。
    struct V1Row {
        id: i64,
        status: String,
        kind: String,
        mp4: Option<String>,
        hls: Option<String>,
    }
    let rows: Vec<V1Row> = tx
        .prepare("SELECT id, status, output_kind, mp4_path, hls_path FROM task ORDER BY id")?
        .query_map([], |row| {
            Ok(V1Row {
                id: row.get(0)?,
                status: row.get(1)?,
                kind: row.get(2)?,
                mp4: row.get(3)?,
                hls: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    // 任务目录的比较键 → 没完成的任务
    let mut active: std::collections::HashMap<String, Vec<i64>> = std::collections::HashMap::new();
    for V1Row {
        id,
        status,
        kind,
        mp4,
        hls,
    } in rows
    {
        let work_dir = match (kind.as_str(), mp4, hls) {
            ("mp4" | "both", Some(mp4), _) => v1_work_dir(&mp4, true),
            ("hls", None, Some(hls)) => v1_work_dir(&hls, false),
            _ => None,
        }
        .ok_or_else(|| Error::Corrupt(format!("任务 {id} 的输出")))?;
        if status == "done" {
            continue;
        }
        let key = naming::path_key(&work_dir);
        active.entry(key.clone()).or_default().push(id);
        tx.execute(
            "UPDATE task SET work_dir = ?2, work_dir_key = ?3 WHERE id = ?1",
            params![id, work_dir, key],
        )?;
    }
    if let Some((key, ids)) = active.iter().find(|(_, ids)| ids.len() > 1) {
        let ids: Vec<String> = ids.iter().map(i64::to_string).collect();
        return Err(Error::Corrupt(format!(
            "任务 {} 共用任务目录 {key}（旧版本取名只看磁盘时可能出现）；放弃其中任何一个都会删掉另一个已下的内容，要先在存储\
             里删掉多余的任务",
            ids.join("、")
        )));
    }
    tx.execute_batch("CREATE UNIQUE INDEX task_work_dir_key ON task (work_dir_key);")?;
    Ok(())
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
const COLUMNS: &str = "id, title, url, preference, headers, output_kind, mp4_path, hls_path, live, status, error, \
                       created_at, total, duration_ms, bytes, width, height";

/// 新建任务，状态为运行中；返回分配的编号。任务目录与已有任务的相同（按 [`naming::path_key`] 比较）时报
/// [`Error::WorkDirTaken`]。
pub fn insert(tx: &Transaction<'_>, spec: &TaskSpec, created_at: i64) -> Result<TaskId, Error> {
    let (kind, mp4, hls) = output_columns(&spec.output)?;
    let work_dir = work_dir_of(&spec.output)?;
    let key = naming::path_key(&work_dir);
    let taken: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM task WHERE work_dir_key = ?1)",
        [&key],
        |row| row.get(0),
    )?;
    if taken {
        return Err(Error::WorkDirTaken(work_dir.into()));
    }
    tx.execute(
        "INSERT INTO task (title, url, preference, headers, output_kind, mp4_path, hls_path, live, status, created_at,
                           work_dir, work_dir_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running', ?9, ?10, ?11)",
        params![
            spec.title,
            spec.url.as_str(),
            to_json(&spec.preference),
            to_json(&spec.headers),
            kind,
            mp4,
            hls,
            spec.live,
            created_at,
            work_dir,
            key
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

/// 已有任务占用的路径的比较键（[`naming::path_key`]）：各任务的 MP4、本地 HLS 目录与任务目录。新建任务选主名时避开
/// 它们。
pub fn taken_paths(tx: &Transaction<'_>) -> Result<HashSet<String>, Error> {
    let mut statement = tx.prepare("SELECT mp4_path, hls_path, work_dir FROM task")?;
    let rows = statement.query_map([], |row| {
        Ok([
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
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

/// 记下点播的总片数；没有这个任务时报 [`Error::NotFound`]。
pub fn set_total(tx: &Transaction<'_>, id: TaskId, total: usize) -> Result<(), Error> {
    let total = i64::try_from(total).expect("分片数远小于 i64 的上限");
    if tx.execute(
        "UPDATE task SET total = ?2 WHERE id = ?1",
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

/// 改状态；没有这个任务时报 [`Error::NotFound`]。
pub fn set_status(tx: &Transaction<'_>, id: TaskId, status: &Status) -> Result<(), Error> {
    let (name, error) = match status {
        Status::Running => ("running", None),
        Status::Paused => ("paused", None),
        Status::Failed(message) => ("failed", Some(message.as_str())),
        Status::Done => ("done", None),
    };
    let changed = tx.execute(
        "UPDATE task SET status = ?2, error = ?3 WHERE id = ?1",
        params![id.0, name, error],
    )?;
    if changed == 0 {
        return Err(Error::NotFound(id));
    }
    Ok(())
}

/// 把还标着运行中的任务改为暂停：应用上次异常退出时没来得及改；返回改了几个。
pub fn pause_interrupted(tx: &Transaction<'_>) -> Result<usize, Error> {
    Ok(tx.execute(
        "UPDATE task SET status = 'paused' WHERE status = 'running'",
        [],
    )?)
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
    title: String,
    url: String,
    preference: String,
    headers: String,
    output_kind: String,
    mp4_path: Option<String>,
    hls_path: Option<String>,
    live: bool,
    status: String,
    error: Option<String>,
    created_at: i64,
    total: Option<i64>,
    duration_ms: Option<i64>,
    bytes: Option<i64>,
    width: Option<u32>,
    height: Option<u32>,
}

impl Row {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
        Ok(Row {
            id: row.get(0)?,
            title: row.get(1)?,
            url: row.get(2)?,
            preference: row.get(3)?,
            headers: row.get(4)?,
            output_kind: row.get(5)?,
            mp4_path: row.get(6)?,
            hls_path: row.get(7)?,
            live: row.get(8)?,
            status: row.get(9)?,
            error: row.get(10)?,
            created_at: row.get(11)?,
            total: row.get(12)?,
            duration_ms: row.get(13)?,
            bytes: row.get(14)?,
            width: row.get(15)?,
            height: row.get(16)?,
        })
    }

    fn into_task(self) -> Result<Task, Error> {
        let id = TaskId(self.id);
        let corrupt = |what: &str| Error::Corrupt(format!("任务 {} 的{what}", self.id));
        let url = self.url.parse().map_err(|_| corrupt("地址"))?;
        let preference: StoredPreference =
            serde_json::from_str(&self.preference).map_err(|_| corrupt("选轨偏好"))?;
        let headers: Vec<(String, String)> =
            serde_json::from_str(&self.headers).map_err(|_| corrupt("请求头"))?;
        let output = output_spec(&self.output_kind, self.mp4_path, self.hls_path)
            .ok_or_else(|| corrupt("输出"))?;
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
        let status = match (self.status.as_str(), self.error) {
            ("running", None) => Status::Running,
            ("paused", None) => Status::Paused,
            ("failed", Some(message)) => Status::Failed(message),
            ("done", None) => Status::Done,
            _ => return Err(corrupt("状态")),
        };
        Ok(Task {
            id,
            spec: TaskSpec {
                title: self.title,
                url,
                preference,
                headers,
                output,
                live: self.live,
            },
            status,
            created_at: self.created_at,
            total,
            result,
        })
    }
}

/// 表里的输出三列读回成输出；种类与路径对不上时为 None。
fn output_spec(kind: &str, mp4: Option<String>, hls: Option<String>) -> Option<OutputSpec> {
    match (kind, mp4, hls) {
        ("mp4", Some(mp4), None) => Some(OutputSpec::Mp4(mp4.into())),
        ("hls", None, Some(hls)) => Some(OutputSpec::Hls(hls.into())),
        ("both", Some(mp4), Some(hls)) => Some(OutputSpec::Both {
            mp4: mp4.into(),
            hls: hls.into(),
        }),
        _ => None,
    }
}

/// 输出对应的任务目录（核心按输出路径算出的绝对路径），在表里的写法。
fn work_dir_of(output: &OutputSpec) -> Result<String, Error> {
    let path = output.options().resolved_work_dir()?;
    path.into_os_string()
        .into_string()
        .map_err(|path| Error::InvalidInput(format!("任务目录路径不是 UTF-8：{}", path.display())))
}

/// 输出在表里的三列：种类与两个路径。路径须是 UTF-8（核心对输出路径的要求相同）。
fn output_columns(
    output: &OutputSpec,
) -> Result<(&'static str, Option<String>, Option<String>), Error> {
    let text = |path: &PathBuf| {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::InvalidInput(format!("输出路径不是 UTF-8：{}", path.display())))
    };
    Ok(match output {
        OutputSpec::Mp4(mp4) => ("mp4", Some(text(mp4)?), None),
        OutputSpec::Hls(hls) => ("hls", None, Some(text(hls)?)),
        OutputSpec::Both { mp4, hls } => ("both", Some(text(mp4)?), Some(text(hls)?)),
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

    fn spec(output: OutputSpec) -> TaskSpec {
        TaskSpec {
            title: "主题演讲".into(),
            url: "https://media.example.com/keynote/master.m3u8?token=1"
                .parse()
                .unwrap(),
            preference: StoredPreference::try_from(&Preference::default()).unwrap(),
            headers: vec![("Referer".into(), "https://example.com/".into())],
            output,
            live: true,
        }
    }

    #[test]
    fn tasks_round_trip_and_change_status() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let a = insert(&tx, &spec(OutputSpec::Mp4("/m/a.mp4".into())), 100).unwrap();
        let b = insert(
            &tx,
            &spec(OutputSpec::Both {
                mp4: "/m/b.mp4".into(),
                hls: "/m/b".into(),
            }),
            200,
        )
        .unwrap();
        set_status(&tx, a, &Status::Failed("403".into())).unwrap();
        set_status(&tx, b, &Status::Done).unwrap();
        tx.commit().unwrap();

        let tx = store.transaction().unwrap();
        let tasks = list(&tx).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].spec, spec(OutputSpec::Mp4("/m/a.mp4".into())));
        assert_eq!(tasks[0].status, Status::Failed("403".into()));
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

    #[test]
    fn interrupted_tasks_become_paused() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let running = insert(&tx, &spec(OutputSpec::Hls("/m/c".into())), 1).unwrap();
        let done = insert(&tx, &spec(OutputSpec::Hls("/m/d".into())), 2).unwrap();
        set_status(&tx, done, &Status::Done).unwrap();
        assert_eq!(pause_interrupted(&tx).unwrap(), 1);
        assert_eq!(get(&tx, running).unwrap().unwrap().status, Status::Paused);
        assert_eq!(get(&tx, done).unwrap().unwrap().status, Status::Done);
    }

    #[test]
    fn a_store_from_a_newer_version_is_refused_and_rows_are_checked() {
        let dir = std::env::temp_dir().join(format!("hs-m3u8-app-store-{}", std::process::id()));
        let path = dir.join("tasks.sqlite");
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        {
            let mut store = Store::open(&path).unwrap();
            let tx = store.transaction().unwrap();
            tx.execute(
                "INSERT INTO task (title, url, preference, headers, output_kind, mp4_path, live, status, created_at)
                 VALUES ('x', 'https://a/b.m3u8', '{}', '[]', 'mp4', '/m/x.mp4', 0, 'done', 1)",
                [],
            )
            .unwrap();
            tx.commit().unwrap();
            let tx = store.transaction().unwrap();
            assert!(matches!(list(&tx), Err(Error::Corrupt(_))));
        }
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

    /// 测试用的空目录，名字带进程号。
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hs-m3u8-app-{name}-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 第 1 版的存储：建表、插入 `rows`（状态、MP4 路径），版本记为 1。
    fn version_1(path: &Path, rows: &[(&str, &str)]) {
        let mut conn = Connection::open(path).unwrap();
        let tx = conn.transaction().unwrap();
        create_task_table(&tx).unwrap();
        for (status, mp4) in rows {
            tx.execute(
                "INSERT INTO task (title, url, preference, headers, output_kind, mp4_path, live, status, error, created_at)
                 VALUES ('a', 'https://a/b.m3u8', ?1, '[]', 'mp4', ?2, 0, ?3, ?4, 1)",
                params![
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

    /// 第 1 版的存储升到当前版本：没完成的任务补上按输出算出的任务目录（之后新建的任务避开它），已完成的不占；
    /// 已完成的与没完成的同名不算冲突。
    #[test]
    fn version_1_rows_get_their_work_dir() {
        let dir = scratch("migrate");
        let path = dir.join("tasks.sqlite");
        version_1(&path, &[("paused", "/m/a.mp4"), ("done", "/m/A.mp4")]);
        let mut store = Store::open(&path).unwrap();
        let tx = store.transaction().unwrap();
        let taken = taken_paths(&tx).unwrap();
        assert!(taken.contains(&naming::path_key("/m/a.hsdl")), "{taken:?}");
        let tasks = list(&tx).unwrap();
        assert_eq!((tasks[0].total, &tasks[1].result), (None, &None));
        drop(tx);
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 第 1 版里两个没完成的任务共用一个任务目录（只差大小写）：迁移报错并指出是哪两个任务，存储不动。
    #[test]
    fn version_1_duplicates_are_reported() {
        let dir = scratch("migrate-duplicates");
        let path = dir.join("tasks.sqlite");
        version_1(&path, &[("paused", "/m/a.mp4"), ("failed", "/m/A.mp4")]);
        match Store::open(&path) {
            Err(Error::Corrupt(message)) => assert!(message.contains("任务 1、2"), "{message}"),
            Err(other) => panic!("{other}"),
            Ok(_) => panic!("迁移应当失败"),
        }
        let version: i64 = Connection::open(&path)
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
        std::fs::remove_dir_all(&dir).unwrap();
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

    /// 任务目录与已有任务相同的拒收；MP4 `a.mp4` 与本地 HLS `a` 的任务目录都是 `a.hsdl`。记下总片数后读得回。
    #[test]
    fn work_dirs_are_unique_and_totals_are_kept() {
        let mut store = Store::in_memory().unwrap();
        let tx = store.transaction().unwrap();
        let a = insert(&tx, &spec(OutputSpec::Mp4("/m/a.mp4".into())), 1).unwrap();
        assert!(matches!(
            insert(&tx, &spec(OutputSpec::Hls("/m/a".into())), 2),
            Err(Error::WorkDirTaken(path)) if path == Path::new("/m/a.hsdl")
        ));
        set_total(&tx, a, 664).unwrap();
        assert_eq!(get(&tx, a).unwrap().unwrap().total, Some(664));
        assert!(matches!(
            set_total(&tx, TaskId(99), 1),
            Err(Error::NotFound(_))
        ));
    }
}
