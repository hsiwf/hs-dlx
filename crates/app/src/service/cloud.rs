//! 云离线任务：提交磁力链接给网盘，等网盘下完，列出文件、定下内容的根，再逐个取直链交给核心分段下载，按网盘给的 GCID
//! 校验，全部通过才算完成。各文件下完即记进存储，暂停或失败后继续时只补没下完的。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hs_dlx_cloud::pikpak::{self, Client, Entry, Phase, Reason, Session, gcid::Gcid};
use hs_m3u8_core::{
    CancellationToken, FileOutputOptions, FileProgress, FileRequest, HttpErrorKind, HttpOptions,
    Proxy, Source,
};
use tokio::task::JoinHandle;

use super::{Event, Shared, lock, throttled};
use crate::naming::{self, CloudPlace};
use crate::store;
use crate::{CloudFile, CloudSpec, Error, Failure, FailureKind, Status, Task, TaskId, TaskSpec};

/// 网盘还在下时多久问一次进度。
const POLL: Duration = Duration::from_secs(5);
/// 直链被拒（过期、换了出口）时最多重取几次。
const RELINKS: u32 = 3;
/// 校验时每次读多少字节。
const VERIFY_CHUNK: usize = 1 << 20;
/// 网盘文件夹最多套几层：防网盘回的目录结构打转。
const MAX_DEPTH: usize = 32;

/// 运行中的云离线任务的进度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudProgress {
    /// 网盘在下：阶段与百分比（0 到 100）
    Remote { phase: Phase, percent: u8 },
    /// 网盘下完了，在列文件
    Listing,
    /// 在下第 `index` 个文件（从 0 起），共 `count` 个；`done` 为之前的文件已下完的字节数，`total` 为全部文件的，
    /// `received` 为这次运行从网络读到的字节数（各文件合计，同 [`FileProgress::received`] 按它的变化算速度）
    Fetching {
        index: usize,
        count: usize,
        done: u64,
        total: u64,
        received: u64,
        file: Box<FileProgress>,
    },
    /// 在校验第 `index` 个文件，已读 `checked` 字节；其余同 [`CloudProgress::Fetching`]
    Verifying {
        index: usize,
        count: usize,
        done: u64,
        total: u64,
        checked: u64,
    },
}

/// 开始了的云离线任务。
pub(super) struct CloudRun {
    pub cancel: CancellationToken,
    pub done: JoinHandle<Outcome>,
}

/// 云离线任务停下的结果。
pub(super) enum Outcome {
    /// 全部文件下完并校验过：字节数
    Finished(u64),
    Paused,
    Failed(Failure),
}

/// 网盘的失败翻译成任务的失败。
pub(super) fn failure(error: &pikpak::Error) -> Failure {
    let kind = match error {
        pikpak::Error::Transport(error) => FailureKind::of(error),
        pikpak::Error::Rejected(rejection) => match rejection.reason() {
            // 换过令牌、重申过 captcha 之后仍被拒
            Reason::LoginExpired | Reason::AccessExpired | Reason::Captcha => {
                FailureKind::CloudLogin
            }
            Reason::Quota => FailureKind::CloudQuota,
            Reason::RateLimited => FailureKind::Status(429),
            Reason::Other => FailureKind::CloudFailed,
        },
        pikpak::Error::Unrecognized(_) => FailureKind::CloudApi,
        pikpak::Error::Verification(_)
        | pikpak::Error::LoginExpired
        | pikpak::Error::LoginDenied => FailureKind::CloudLogin,
        pikpak::Error::Save(_) => FailureKind::Disk,
        pikpak::Error::NoRandom(_) => FailureKind::Other,
    };
    let message = match error {
        pikpak::Error::Verification(url) => {
            format!("PikPak 要求人机验证：在浏览器里打开 {url} 通过后再继续")
        }
        error => error.to_string(),
    };
    Failure { kind, message }
}

impl Shared {
    /// PikPak 的登录状态：还没有时照存储里的账号建；没登录时报 [`Error::NotLoggedIn`]。
    pub(super) fn pikpak_session(self: &Arc<Self>) -> Result<Arc<Session>, Error> {
        let mut session = lock(&self.pikpak);
        if let Some(session) = &*session {
            return Ok(session.clone());
        }
        let account = {
            let mut store = lock(&self.store);
            let tx = store.transaction()?;
            store::pikpak_account(&tx)?
        }
        .ok_or(Error::NotLoggedIn)?;
        let created = Arc::new(self.new_pikpak_session(account));
        *session = Some(created.clone());
        Ok(created)
    }

    /// 以 `account` 建 PikPak 的登录状态：换了的令牌写进存储。
    pub(super) fn new_pikpak_session(self: &Arc<Self>, account: pikpak::Account) -> Session {
        let shared = Arc::downgrade(self);
        Session::new(
            self.pikpak_endpoints.clone(),
            account,
            Box::new(move |account| {
                let shared = shared.upgrade().ok_or("应用正在退出")?;
                shared
                    .write(|tx| store::set_pikpak_account(tx, Some(account)))
                    .map_err(|error| error.to_string())
            }),
        )
    }

    /// 经代理 `proxy` 调 PikPak 接口的客户端。
    pub(super) fn pikpak_client(self: &Arc<Self>, proxy: Proxy) -> Result<Client, Error> {
        let http = HttpOptions {
            proxy,
            ..HttpOptions::default()
        };
        Ok(Client::new(
            self.engine.clone(),
            http,
            self.pikpak_session()?,
        ))
    }

    /// 开始云离线任务 `task`（经代理 `proxy`，取直链与下载走同一出口）；调用方须在运行时里。
    pub(super) fn launch_cloud(
        self: &Arc<Self>,
        task: &Task,
        proxy: Proxy,
    ) -> Result<CloudRun, Error> {
        let client = self.pikpak_client(proxy.clone())?;
        let cancel = CancellationToken::new();
        let shared = self.clone();
        let (task, token) = (task.clone(), cancel.clone());
        let done = tokio::spawn(async move {
            let ctx = Ctx {
                client: &client,
                proxy: &proxy,
                cancel: &token,
            };
            match shared.run_cloud(&task, ctx).await {
                Ok(bytes) => Outcome::Finished(bytes),
                Err(Stop::Paused) => Outcome::Paused,
                Err(Stop::Failed(failure)) => Outcome::Failed(failure),
            }
        });
        Ok(CloudRun { cancel, done })
    }

    async fn run_cloud(self: &Arc<Self>, task: &Task, ctx: Ctx<'_>) -> Result<u64, Stop> {
        let id = task.id;
        let spec = task.spec.cloud().expect("只为云离线任务开始").clone();
        let (root, work_dir, files) = match (&spec.root, &task.work_dir) {
            (Some(root), Some(work_dir)) => {
                let files = self
                    .write(|tx| store::cloud_files(tx, id))
                    .map_err(Stop::store)?;
                (root.clone(), work_dir.clone(), files)
            }
            (None, None) => self.list_cloud(id, &spec, ctx).await?,
            _ => unreachable!("存储的约束保证根与任务目录一起定下"),
        };
        let count = files.len();
        let total: u64 = files.iter().map(|file| file.size).sum();
        let mut done: u64 = files
            .iter()
            .filter(|file| file.done)
            .map(|file| file.size)
            .sum();
        let mut received = 0;
        for (index, file) in files.iter().enumerate() {
            if file.done {
                continue;
            }
            let output = output_of(&root, &file.path);
            let at = Position {
                id,
                index,
                count,
                done,
                total,
                received,
            };
            if !exists(&output).await.map_err(Stop::Failed)? {
                let file_dir = work_dir.join(index.to_string());
                received += self
                    .fetch_cloud_file(at, file, &output, &file_dir, ctx)
                    .await?;
            }
            self.verify_cloud_file(at, file, &output, ctx.cancel)
                .await?;
            self.write(|tx| store::set_cloud_file_done(tx, id, index, true))
                .map_err(Stop::store)?;
            done += file.size;
        }
        // 各文件的任务目录装上输出后已删，剩下空的父目录
        let removed = tokio::fs::remove_dir(&work_dir).await;
        if let Err(error) = removed
            && error.kind() != std::io::ErrorKind::NotFound
        {
            // 输出都已校验过，留下的空目录不影响结果
            let _closed = self.events.send(Event::Orphaned {
                paths: vec![work_dir],
                reason: format!("删不掉：{error}"),
            });
        }
        Ok(total)
    }

    /// 等网盘下完，列出文件，定下内容的根与任务目录并记进存储；返回根、任务目录与文件。
    async fn list_cloud(
        self: &Arc<Self>,
        id: TaskId,
        spec: &CloudSpec,
        ctx: Ctx<'_>,
    ) -> Result<(PathBuf, PathBuf, Vec<CloudFile>), Stop> {
        let (client, cancel) = (ctx.client, ctx.cancel);
        let job = loop {
            let job = until(cancel, client.job(&spec.job))
                .await?
                .map_err(Stop::cloud)?;
            let Some(job) = job else {
                return Err(Stop::failed(
                    FailureKind::CloudFailed,
                    "网盘上的这个离线任务不在了（可能在网页或 App 里删了），只能放弃".to_owned(),
                ));
            };
            self.cloud_progress(
                id,
                CloudProgress::Remote {
                    phase: job.phase,
                    percent: job.progress,
                },
            );
            match job.phase {
                Phase::Complete => break job,
                Phase::Failed => {
                    let reason = if job.message.is_empty() {
                        "网盘没有说原因".to_owned()
                    } else {
                        job.message
                    };
                    return Err(Stop::failed(
                        FailureKind::CloudFailed,
                        format!("网盘上的离线下载失败了：{reason}"),
                    ));
                }
                Phase::Pending | Phase::Running | Phase::Paused => {
                    until(cancel, tokio::time::sleep(POLL)).await?;
                }
            }
        };
        self.cloud_progress(id, CloudProgress::Listing);
        let top_id = job.file.ok_or_else(|| {
            Stop::failed(
                FailureKind::CloudApi,
                "网盘说离线下载完成了，却没给出下到的文件".to_owned(),
            )
        })?;
        let top = until(cancel, client.entry(&top_id))
            .await?
            .map_err(Stop::cloud)?;
        let mut found = Vec::new();
        if top.folder {
            until(cancel, walk(client, &top, &mut Vec::new(), &mut found))
                .await?
                .map_err(Stop::cloud)?;
        } else {
            found.push((Vec::new(), top.clone()));
        }
        if found.is_empty() {
            return Err(Stop::failed(
                FailureKind::CloudFailed,
                "网盘上下到的文件夹是空的".to_owned(),
            ));
        }
        let places: Vec<CloudPlace> = found
            .iter()
            .map(|(folders, entry)| CloudPlace {
                folders: folders.clone(),
                file: (entry.name.clone(), entry.id.clone()),
            })
            .collect();
        let paths = if top.folder {
            naming::cloud_paths(&places)
        } else {
            vec![String::new()]
        };
        let files: Vec<CloudFile> = found
            .into_iter()
            .zip(paths)
            .map(|((_, entry), path)| CloudFile {
                id: entry.id,
                path,
                size: entry.size,
                gcid: entry.gcid.expect("文件都有 GCID（读出时已核对）"),
                done: false,
            })
            .collect();
        // 取名与写入之间别的任务可能占了同一个根：写入时报 PathTaken 就重新取名（同新建）
        loop {
            let taken = self.write(store::taken_paths).map_err(Stop::store)?;
            let seen = taken.clone();
            let (folder, name, single, fallback) = (
                spec.folder.clone(),
                top.name.clone(),
                !top.folder,
                spec.job.clone(),
            );
            let (_, root) = super::named(&spec.folder, move || {
                naming::cloud_root_for(&folder, &name, single, &fallback, &taken)
            })
            .await
            .map_err(Stop::store)?;
            let work_dir = naming::file_work_dir(&root);
            match self.write(|tx| store::set_cloud_root(tx, id, &root, &work_dir, &files)) {
                Ok(()) => {
                    // 标题换成了根名
                    let _closed = self.events.send(Event::Renamed { id });
                    return Ok((root, work_dir, files));
                }
                Err(Error::PathTaken(path))
                    if !seen.contains(&naming::path_key(&path.to_string_lossy())) => {}
                Err(error) => return Err(Stop::store(error)),
            }
        }
    }

    /// 取直链、交给核心下载第 `at.index` 个文件到 `output`（任务目录 `file_dir`）；直链被拒时重取，最多 [`RELINKS`] 次。
    /// 返回从网络读到的字节数（各次合计）。
    async fn fetch_cloud_file(
        self: &Arc<Self>,
        at: Position,
        file: &CloudFile,
        output: &Path,
        file_dir: &Path,
        ctx: Ctx<'_>,
    ) -> Result<u64, Stop> {
        let (client, proxy, cancel) = (ctx.client, ctx.proxy, ctx.cancel);
        let mut relinked = 0;
        // 之前几次（直链被拒前）读到的
        let mut received = 0;
        loop {
            let entry = until(cancel, client.entry(&file.id))
                .await?
                .map_err(Stop::cloud)?;
            let link = entry.link.ok_or_else(|| {
                Stop::failed(
                    FailureKind::CloudApi,
                    format!("网盘没给出 {} 的直链", file.path),
                )
            })?;
            let mut source = Source::new(link);
            source.http.proxy = proxy.clone();
            let mut options = FileOutputOptions::new(output.to_path_buf());
            options.work_dir = Some(file_dir.to_path_buf());
            let mut request = FileRequest::new(source, options);
            request.expected = Some(hs_m3u8_core::Expected {
                length: Some(file.size),
                ..hs_m3u8_core::Expected::default()
            });
            let job = self.engine.start_file(request).map_err(Stop::core)?;
            let control = job.control();
            let finished = CancellationToken::new();
            let events = self.events.clone();
            let before = at.received + received;
            let forwarding = tokio::spawn(throttled(
                control.progress(),
                finished.clone(),
                move |progress: FileProgress| {
                    // 接收端随界面关闭而丢弃时没有谁要
                    let _closed = events.send(Event::CloudProgress {
                        id: at.id,
                        progress: Box::new(CloudProgress::Fetching {
                            index: at.index,
                            count: at.count,
                            done: at.done,
                            total: at.total,
                            received: before + progress.received,
                            file: Box::new(progress),
                        }),
                    });
                },
            ));
            let waiting = job.wait();
            tokio::pin!(waiting);
            let result = tokio::select! {
                result = &mut waiting => result,
                () = cancel.cancelled() => {
                    control.cancel();
                    waiting.await
                }
            };
            finished.cancel();
            forwarding
                .await
                .expect("转发进度的任务不会 panic，运行时关闭时本任务也一起丢弃");
            received += control.progress().borrow().received;
            match result {
                Ok(_) => return Ok(received),
                Err(hs_m3u8_core::Error::Cancelled) => return Err(Stop::Paused),
                Err(error) if link_rejected(&error) && relinked < RELINKS => relinked += 1,
                Err(error) => return Err(Stop::core(error)),
            }
        }
    }

    /// 按网盘给的 GCID 校验第 `at.index` 个文件 `output`；不符时删掉它、以 [`FailureKind::Integrity`] 失败。
    async fn verify_cloud_file(
        self: &Arc<Self>,
        at: Position,
        file: &CloudFile,
        output: &Path,
        cancel: &CancellationToken,
    ) -> Result<(), Stop> {
        let (path, size, cancelled) = (output.to_path_buf(), file.size, cancel.clone());
        let events = self.events.clone();
        let checked = tokio::task::spawn_blocking(move || {
            gcid_of(&path, size, &cancelled, |checked| {
                // 接收端随界面关闭而丢弃时没有谁要
                let _closed = events.send(Event::CloudProgress {
                    id: at.id,
                    progress: Box::new(CloudProgress::Verifying {
                        index: at.index,
                        count: at.count,
                        done: at.done,
                        total: at.total,
                        checked,
                    }),
                });
            })
        })
        .await
        .expect("校验不会 panic，运行时关闭时本任务也一起丢弃")?;
        if checked == file.gcid {
            return Ok(());
        }
        let removed = tokio::fs::remove_file(output).await;
        let note = match removed {
            Ok(()) => "已删掉，继续时重下这个文件".to_owned(),
            Err(error) => format!("删不掉（{error}），先手动删掉它再继续"),
        };
        Err(Stop::failed(
            FailureKind::Integrity,
            format!(
                "{} 与网盘给的 GCID 不符（网盘 {}，下到的 {checked}），{note}",
                output.display(),
                file.gcid
            ),
        ))
    }

    fn cloud_progress(&self, id: TaskId, progress: CloudProgress) {
        // 接收端随界面关闭而丢弃时没有谁要
        let _closed = self.events.send(Event::CloudProgress {
            id,
            progress: Box::new(progress),
        });
    }

    /// 放弃云离线任务的文件：各文件的核心任务目录、已装上的输出，再删空了的目录（内容的根、子目录与任务目录）；
    /// 删不掉、或里面有不是这个任务的东西时报 [`Error::ContentLeft`]。
    pub(super) async fn discard_cloud(
        self: &Arc<Self>,
        id: TaskId,
        spec: &CloudSpec,
        work_dir: Option<&Path>,
    ) -> Result<(), Error> {
        let (Some(root), Some(work_dir)) = (&spec.root, work_dir) else {
            return Ok(());
        };
        let files = self.write(|tx| store::cloud_files(tx, id))?;
        let left = |reason: String| Error::ContentLeft {
            id,
            cause: Box::new(Error::InvalidInput(reason)),
        };
        for index in 0..files.len() {
            let dir = work_dir.join(index.to_string());
            let leftovers = self
                .engine
                .discard(
                    &dir,
                    hs_m3u8_core::Unsettled::Fail,
                    &CancellationToken::new(),
                )
                .await?;
            if let Some(leftover) = leftovers.first() {
                return Err(left(leftover.to_string()));
            }
        }
        for file in &files {
            let output = output_of(root, &file.path);
            match tokio::fs::remove_file(&output).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(left(format!("删不掉 {}：{error}", output.display()))),
            }
        }
        // 由深到浅删空了的目录：先各文件所在的子目录，再内容的根（单文件时根就是文件，上面已删）与任务目录
        let mut dirs: Vec<PathBuf> = files
            .iter()
            .filter(|file| !file.path.is_empty())
            .flat_map(|file| {
                let output = output_of(root, &file.path);
                output
                    .ancestors()
                    .skip(1)
                    .take_while(|dir| dir.starts_with(root))
                    .map(Path::to_path_buf)
                    .collect::<Vec<_>>()
            })
            .collect();
        dirs.sort_by_key(|dir| std::cmp::Reverse(dir.components().count()));
        dirs.dedup();
        dirs.push(work_dir.to_path_buf());
        for dir in dirs {
            match tokio::fs::remove_dir(&dir).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(left(format!(
                        "删不掉 {}（里面可能有不是这个任务的文件）：{error}",
                        dir.display()
                    )));
                }
            }
        }
        Ok(())
    }
}

/// 一次运行里各步共用的：调网盘的客户端、下载经的代理（与取直链同一出口）、暂停的信号。
#[derive(Clone, Copy)]
struct Ctx<'a> {
    client: &'a Client,
    proxy: &'a Proxy,
    cancel: &'a CancellationToken,
}

/// 一个文件在任务里的位置，报进度用。
#[derive(Debug, Clone, Copy)]
struct Position {
    id: TaskId,
    index: usize,
    count: usize,
    done: u64,
    total: u64,
    /// 这次运行之前的文件从网络读到的字节数
    received: u64,
}

/// 云离线任务没跑完的原因。
enum Stop {
    Paused,
    Failed(Failure),
}

impl Stop {
    fn failed(kind: FailureKind, message: String) -> Stop {
        Stop::Failed(Failure { kind, message })
    }

    fn cloud(error: pikpak::Error) -> Stop {
        Stop::Failed(failure(&error))
    }

    fn core(error: hs_m3u8_core::Error) -> Stop {
        Stop::Failed(Failure::of(&error))
    }

    /// 存储读写或取名出错：任务停下，以磁盘一类的失败报出（处理后可再继续）。
    fn store(error: Error) -> Stop {
        Stop::failed(FailureKind::Disk, error.to_string())
    }
}

/// 等 `future`，`cancel` 先到时为 [`Stop::Paused`]。
async fn until<T>(cancel: &CancellationToken, future: impl Future<Output = T>) -> Result<T, Stop> {
    cancel.run_until_cancelled(future).await.ok_or(Stop::Paused)
}

/// 递归列出网盘文件夹 `folder` 里的文件，连同各自所在的各级文件夹（`path`，名字与编号）放进 `found`。
async fn walk(
    client: &Client,
    folder: &Entry,
    path: &mut Vec<(String, String)>,
    found: &mut Vec<(Vec<(String, String)>, Entry)>,
) -> Result<(), pikpak::Error> {
    if path.len() >= MAX_DEPTH {
        return Err(pikpak::Error::Unrecognized(format!(
            "文件夹套了 {MAX_DEPTH} 层以上"
        )));
    }
    for entry in client.children(&folder.id).await? {
        if entry.folder {
            path.push((entry.name.clone(), entry.id.clone()));
            Box::pin(walk(client, &entry, path, found)).await?;
            path.pop();
        } else {
            found.push((path.clone(), entry));
        }
    }
    Ok(())
}

/// 文件在内容的根 `root` 下的位置：`path` 为空时就是根。
fn output_of(root: &Path, path: &str) -> PathBuf {
    if path.is_empty() {
        root.to_path_buf()
    } else {
        path.split('/')
            .fold(root.to_path_buf(), |dir, part| dir.join(part))
    }
}

/// `path` 上已有东西（上次下完、装上了输出，还没校验就停了）。
async fn exists(path: &Path) -> Result<bool, Failure> {
    tokio::fs::try_exists(path).await.map_err(|error| Failure {
        kind: FailureKind::Disk,
        message: format!("查不了 {}：{error}", path.display()),
    })
}

/// 直链被拒：过期了或换了出口，重取一个再下。
fn link_rejected(error: &hs_m3u8_core::Error) -> bool {
    match error {
        hs_m3u8_core::Error::Http {
            kind: HttpErrorKind::Status(status),
            ..
        } => matches!(status, 401 | 403 | 410),
        hs_m3u8_core::Error::NotExpected { .. } => true,
        _ => false,
    }
}

/// 读 `path` 算 GCID（总长 `size` 字节），每读一块调 `checked`（已读字节数）；`cancel` 取消时为 [`Stop::Paused`]，文件
/// 长度不是 `size` 时以 [`FailureKind::Integrity`] 失败。
fn gcid_of(
    path: &Path,
    size: u64,
    cancel: &CancellationToken,
    mut checked: impl FnMut(u64),
) -> Result<String, Stop> {
    use std::io::Read as _;
    let disk = |error: std::io::Error| {
        Stop::failed(
            FailureKind::Disk,
            format!("读不了 {}：{error}", path.display()),
        )
    };
    let mut reader = std::fs::File::open(path).map_err(disk)?;
    let mut gcid = Gcid::new(size);
    let mut buffer = vec![0; VERIFY_CHUNK];
    let mut read: u64 = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(Stop::Paused);
        }
        let n = reader.read(&mut buffer).map_err(disk)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        gcid.update(&buffer[..n]);
        checked(read);
    }
    if read != size {
        return Err(Stop::failed(
            FailureKind::Integrity,
            format!("{} 有 {read} 字节，网盘上是 {size} 字节", path.display()),
        ));
    }
    Ok(gcid.finish())
}

/// 新建云离线任务时的规格：标题取网盘给的名字（整理成文件主名），什么都不剩时为「磁力链接」。
pub(super) fn draft_spec(spec: CloudSpec, name: &str, url: hs_m3u8_core::Url) -> TaskSpec {
    let title = naming::clean_title(name);
    let title = if title.is_empty() {
        "磁力链接".to_owned()
    } else {
        title
    };
    TaskSpec {
        title,
        url,
        headers: Vec::new(),
        kind: crate::Kind::Cloud(spec),
    }
}

/// 云离线任务的结束状态。
pub(super) fn ending(outcome: Outcome) -> (Status, Option<crate::Finished>) {
    match outcome {
        Outcome::Finished(bytes) => (
            Status::Done,
            Some(crate::Finished {
                duration: None,
                bytes,
                resolution: None,
            }),
        ),
        Outcome::Paused => (Status::Paused, None),
        Outcome::Failed(failure) => (Status::Failed(failure), None),
    }
}
