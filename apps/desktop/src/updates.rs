//! 应用的更新：设置里开着自动检查时，启动后与之后每天查一次 GitHub 上的最新版本；有新版本时顶栏提示，设置页里下载、
//! 验签、装上，再照常退出（有任务在运行时问）。这次退出不可撤回时起接着更新的进程（见 [`hs_dlx_app::Restart`]），由它
//! 打开新版本；退出被取消时停在「已装好」，可再点重启。窗口关着时照常进行。

use std::time::Duration;

use gpui_kit::{Context, Entity, Subscription, Task};
use hs_dlx_app::{Release, Restart, UpdateFeed, Version};

use crate::tasks::Tasks;

/// 启动后多久查第一次：不和启动时读任务、开窗口抢。
const FIRST_CHECK: Duration = Duration::from_secs(10);
/// 之后多久查一次。
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 发布的更新在哪：GitHub 上本仓库的最新 Release，安装包的签名用 update.pub 里的公钥验。调试构建里同时设了环境变量
/// `HS_DLX_UPDATE_FEED`（最新 Release 的地址）与 `HS_DLX_UPDATE_KEY`（minisign 公钥）时换成它们，用于在本机走一遍
/// 更新；发布构建只认这里写死的。
pub fn feed() -> UpdateFeed {
    #[cfg(debug_assertions)]
    if let (Ok(latest), Ok(public_key)) = (
        std::env::var("HS_DLX_UPDATE_FEED"),
        std::env::var("HS_DLX_UPDATE_KEY"),
    ) {
        return UpdateFeed {
            latest: hs_m3u8_core::Url::parse(&latest).expect("HS_DLX_UPDATE_FEED 要是地址"),
            public_key,
            dir: download_dir(),
        };
    }
    UpdateFeed {
        latest: hs_m3u8_core::Url::parse(
            "https://api.github.com/repos/x-haose/hs-dlx/releases/latest",
        )
        .expect("写死的地址有效"),
        public_key: include_str!("../update.pub")
            .lines()
            .nth(1)
            .expect("minisign 公钥文件的第二行是公钥")
            .to_owned(),
        dir: download_dir(),
    }
}

/// 下载的安装包放在系统的临时目录下。
fn download_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("hs-dlx-update")
}

/// 更新进行到哪一步。
#[derive(Clone)]
pub enum UpdateState {
    /// 还没查过
    Idle,
    Checking,
    /// 查过，已是最新
    Latest,
    /// 有新版本
    Available(Release),
    /// 在下载 `release` 的安装包，已下 `received` 字节
    Downloading {
        release: Release,
        received: u64,
    },
    /// 下完验过签名，在装
    Installing(Version),
    /// 装好了：macOS 已换上新版本；Windows 已下载并验过，退出时装（见 [`hs_dlx_app::INSTALLS_ON_EXIT`]）
    Installed(Version),
    /// 这一步没成：`release` 为查到的新版本（查的时候没成为 None），可再试
    Failed {
        release: Option<Release>,
        reason: String,
    },
    /// 换到一半，新旧两份都不在原处：说明里写明各在哪，要手动处理，不再自动更新
    Stranded(String),
}

pub struct Updates {
    tasks: Entity<Tasks>,
    feed: UpdateFeed,
    state: UpdateState,
    /// 自动检查（设置里的）
    auto: bool,
    /// 装好后退出时要起的（[`UpdateState::Installed`] 时有）
    restart: Option<Restart>,
    /// 点了重启、还没撤回的那次退出：撤回前的 [`Tasks::stays`]。退出不可撤回时起 `restart` 并打开新版本；撤回即清掉
    restarting: Option<u64>,
    /// 在做的那一步（查、下载、装）；释放即取消
    _work: Option<Task<()>>,
    _schedule: Task<()>,
    _subscriptions: [Subscription; 2],
}

impl Updates {
    /// `auto` 为设置里存着的「自动检查更新」。
    pub fn new(tasks: Entity<Tasks>, feed: UpdateFeed, auto: bool, cx: &mut Context<Self>) -> Self {
        let schedule = cx.spawn(async move |this, cx| {
            let mut wait = FIRST_CHECK;
            loop {
                cx.background_executor().timer(wait).await;
                wait = CHECK_INTERVAL;
                let alive = this.update(cx, |this, cx| {
                    if this.auto && this.checkable() {
                        this.check(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        // 点了重启而退出被撤回（问的时候选了留下、等任务停下时出了要看的事）：之后别的退出不打开新版本
        let watched = cx.observe(&tasks, |this: &mut Self, tasks, cx| {
            if this
                .restarting
                .is_some_and(|stays| tasks.read(cx).stays() != stays)
            {
                this.restarting = None;
                cx.notify();
            }
        });
        // 退出已不可撤回：装好了的照它接着做，点了重启的另打开新版本
        let quitting = cx.on_app_quit(|this: &mut Self, _| {
            if let Some(restart) = this.restart.take()
                && let Err(error) = restart.spawn(this.restarting.is_some())
            {
                // 已在退出，界面没了：下次启动时报出来
                let _unrecorded =
                    hs_dlx_app::record_update_failure(&this.feed.dir, &error.to_string());
            }
            async {}
        });
        Updates {
            tasks,
            feed,
            state: UpdateState::Idle,
            auto,
            restart: None,
            restarting: None,
            _work: None,
            _schedule: schedule,
            _subscriptions: [watched, quitting],
        }
    }

    pub fn state(&self) -> &UpdateState {
        &self.state
    }

    pub fn auto(&self) -> bool {
        self.auto
    }

    /// 点了重启、在等退出。
    pub fn restarting(&self) -> bool {
        self.restarting.is_some()
    }

    /// 可以（重新）查：没查过、已是最新，或上次查没成。
    fn checkable(&self) -> bool {
        matches!(
            self.state,
            UpdateState::Idle | UpdateState::Latest | UpdateState::Failed { release: None, .. }
        )
    }

    /// 换「自动检查更新」：存进了才换，改成自动时随即查一次；没存进时为原因。
    pub fn set_auto(&mut self, auto: bool, cx: &mut Context<Self>) -> Result<(), String> {
        self.tasks
            .read(cx)
            .service()
            .set_check_updates(auto)
            .map_err(|error| error.to_string())?;
        self.auto = auto;
        if auto && self.checkable() {
            self.check(cx);
        }
        cx.notify();
        Ok(())
    }

    /// 有待用户处理的新版本（顶栏提示它）。
    pub fn pending(&self) -> Option<Version> {
        match &self.state {
            UpdateState::Available(release)
            | UpdateState::Downloading { release, .. }
            | UpdateState::Failed {
                release: Some(release),
                ..
            } => Some(release.version),
            UpdateState::Installing(version) | UpdateState::Installed(version) => Some(*version),
            UpdateState::Idle
            | UpdateState::Checking
            | UpdateState::Latest
            | UpdateState::Failed { release: None, .. }
            | UpdateState::Stranded(_) => None,
        }
    }

    /// 查有没有新版本；不是可以查的时候（见 [`Updates::checkable`]）什么也不做。
    pub fn check(&mut self, cx: &mut Context<Self>) {
        if !self.checkable() {
            return;
        }
        let handle = match self.tasks.read(cx).service().check_update(&self.feed) {
            Ok(handle) => handle,
            Err(error) => {
                self.fail(None, error.to_string(), cx);
                return;
            }
        };
        self.state = UpdateState::Checking;
        self._work = Some(cx.spawn(async move |this, cx| {
            let result = handle.await;
            // 关了时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(Ok(Some(release))) => UpdateState::Available(release),
                    Ok(Ok(None)) => UpdateState::Latest,
                    Ok(Err(error)) => UpdateState::Failed {
                        release: None,
                        reason: error.to_string(),
                    },
                    Err(error) => UpdateState::Failed {
                        release: None,
                        reason: format!("查更新的任务没做完：{error}"),
                    },
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn fail(&mut self, release: Option<Release>, reason: String, cx: &mut Context<Self>) {
        self.state = UpdateState::Failed { release, reason };
        self._work = None;
        cx.notify();
    }

    /// 下载新版本的安装包、验签名、装上，再重启（见 [`Updates::restart_now`]）。不是有新版本可装（或上次没成可再试）时
    /// 什么也不做。
    pub fn install(&mut self, cx: &mut Context<Self>) {
        let release = match &self.state {
            UpdateState::Available(release)
            | UpdateState::Failed {
                release: Some(release),
                ..
            } => release.clone(),
            _ => return,
        };
        let download = match self
            .tasks
            .read(cx)
            .service()
            .download_update(&self.feed, &release)
        {
            Ok(download) => download,
            Err(error) => {
                self.fail(Some(release), error.to_string(), cx);
                return;
            }
        };
        self.state = UpdateState::Downloading {
            release: release.clone(),
            received: 0,
        };
        let mut received = download.received;
        let done = download.done;
        self._work = Some(cx.spawn(async move |this, cx| {
            let progress = {
                let this = this.clone();
                cx.spawn(async move |cx| {
                    while received.changed().await.is_ok() {
                        let bytes = *received.borrow_and_update();
                        let alive = this.update(cx, |this, cx| {
                            if let UpdateState::Downloading { received, .. } = &mut this.state {
                                *received = bytes;
                                cx.notify();
                            }
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                })
            };
            let downloaded = done.await;
            drop(progress);
            let downloaded = match downloaded {
                Ok(Ok(downloaded)) => downloaded,
                Ok(Err(error)) => {
                    let _closed = this.update(cx, |this, cx| {
                        this.fail(Some(release), error.to_string(), cx)
                    });
                    return;
                }
                Err(error) => {
                    let _closed = this.update(cx, |this, cx| {
                        this.fail(Some(release), format!("下载更新的任务没做完：{error}"), cx)
                    });
                    return;
                }
            };
            let version = downloaded.version;
            let installing = this.update(cx, |this, cx| {
                this.state = UpdateState::Installing(version);
                cx.notify();
                this.tasks.read(cx).service().install_update(downloaded)
            });
            let Ok(installing) = installing else {
                return;
            };
            let installed = installing.await;
            let _closed = this.update(cx, |this, cx| match installed {
                Ok(Ok(restart)) => {
                    this.state = UpdateState::Installed(version);
                    this.restart = Some(restart);
                    this._work = None;
                    this.restart_now(cx);
                }
                Ok(Err(hs_dlx_app::Error::UpdateStranded(reason))) => {
                    // 应用已不在原处：不只写在设置页，另推一条提示，免得没看到就退出了
                    this.tasks.update(cx, |tasks, cx| {
                        tasks.tell(format!("更新没换完：{reason}"), cx);
                    });
                    this.state = UpdateState::Stranded(reason);
                    this._work = None;
                    cx.notify();
                }
                Ok(Err(error)) => this.fail(Some(release), error.to_string(), cx),
                Err(error) => this.fail(Some(release), format!("装更新的任务没做完：{error}"), cx),
            });
        }));
        cx.notify();
    }

    /// 装好后重启：照常退出（有任务在运行时问，同退出应用），退出不可撤回时打开新版本。还没装好时什么也不做。
    pub fn restart_now(&mut self, cx: &mut Context<Self>) {
        if self.restart.is_none() {
            return;
        }
        self.restarting = Some(self.tasks.read(cx).stays());
        cx.notify();
        crate::main_window::quit(cx);
    }
}
