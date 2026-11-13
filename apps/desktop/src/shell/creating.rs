//! 新建：探测链接、开新建视图、新建任务并把结果交回发起它的视图。

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui_kit::{AppContext as _, Context, Entity, Window};
use hs_m3u8_app::{Draft, Event};
use hs_m3u8_core::Url;

use super::Shell;
use super::row::Row;
use crate::links;
use crate::new_task::{NewTask, NewTaskEvent};
use crate::sniff::{SniffEvent, SniffView};

impl Shell {
    pub(super) fn probe(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
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
        // 不是 m3u8 的网页地址：在嗅探页里打开，找页面里的视频流
        if links::stream_url(url.as_str()).is_none() {
            self.open_sniff(url, window, cx);
            return;
        }
        self.open_new_task(url, Vec::new(), window, cx);
    }

    /// 打开新建页探测 `url`，带着请求头 `headers`；嗅探页开着时先把它的网页藏起来。
    pub(super) fn open_new_task(
        &mut self,
        url: Url,
        headers: Vec<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((sniff, _)) = &self.sniff {
            sniff.update(cx, |sniff, cx| sniff.hide(cx));
        }
        let (service, folder, home) =
            (self.service.clone(), self.folder.clone(), self.home.clone());
        let view = cx.new(|cx| NewTask::new(service, url, headers, folder, home, window, cx));
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
    pub(super) fn probe_url(&mut self, url: &Url, window: &mut Window, cx: &mut Context<Self>) {
        let text = url.to_string();
        self.paste
            .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
        self.probe(&text, window, cx);
    }

    /// 按新建视图里的选择新建任务；退出中不新建。新建要查磁盘，在服务的运行时里做，期间新建视图显示正在开始。
    pub(super) fn create(
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
                // 没有在途的新建了，留着的结束事件都不属于将要加进来的行（是已移除的行的）
                if this.creating == 0 {
                    this.early.clear();
                }
                this.quit_if_idle(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 新建视图 `view` 发起的任务建成了：加进列表。`view` 还开着（用户没切走）时关掉它、选中新任务、清空粘贴栏；
    /// 选了退出的随即暂停。
    pub(super) fn started(
        &mut self,
        view: &Entity<NewTask>,
        task: hs_m3u8_app::Task,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = task.id;
        let mut row = Row::new(task, Instant::now());
        row.timeline.begin(Duration::ZERO);
        self.rows.push(row);
        let (early, rest) = std::mem::take(&mut self.early)
            .into_iter()
            .partition(|event| matches!(event, Event::Ended { id: at, .. } if *at == id));
        self.early = rest;
        for event in early {
            self.apply(event, cx);
        }
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

    /// 新建失败：`view` 还开着时写在它上面，否则（用户已切走）提示；退出中的不退出，留给用户看。
    pub(super) fn create_failed(
        &mut self,
        view: &Entity<NewTask>,
        error: String,
        cx: &mut Context<Self>,
    ) {
        if self.quitting {
            self.cancel_quit("有新建没成，没有退出".to_owned());
        }
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
    pub(super) fn showing(&self, view: &Entity<NewTask>) -> bool {
        self.new_task
            .as_ref()
            .is_some_and(|(shown, _)| shown == view)
    }

    /// 在嗅探页里打开网页 `url`（已开着的换成这个）。
    fn open_sniff(&mut self, url: Url, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| SniffView::new(url, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this: &mut Self, _, event: &SniffEvent, window, cx| {
                match event {
                    SniffEvent::Probe { url, headers } => {
                        this.open_new_task(url.clone(), headers.clone(), window, cx)
                    }
                    SniffEvent::Dismissed => this.sniff = None,
                }
                cx.notify();
            },
        );
        self.new_task = None;
        self.sniff = Some((view, subscription));
        cx.notify();
    }
}
