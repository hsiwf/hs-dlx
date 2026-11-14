//! 新建：探测链接、开新建视图、新建任务并把结果交回发起它的视图。

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui_kit::{AppContext as _, Context, Entity, Focusable as _, Window};
use hs_m3u8_app::{Draft, Event};
use hs_m3u8_core::Url;

use super::row::Row;
use super::{Page, Shell};
use crate::links;
use crate::new_task::{Link, NewTask, NewTaskEvent, Phase};
use crate::sniff::{SniffEvent, SniffView, Verdict};

impl Shell {
    pub(super) fn probe(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(url) = links::http_url(text) else {
            self.input_error = Some(links::NOT_HTTP.to_owned());
            cx.notify();
            return;
        };
        self.input_error = None;
        self.confirm_discard = None;
        if self.clipboard.as_ref() == Some(&url) {
            self.clipboard = None;
        }
        self.open_new_task(
            Link {
                url,
                title: None,
                headers: Vec::new(),
                probe: None,
            },
            false,
            window,
            cx,
        );
    }

    /// 打开新建页探测 `link`（已开着的换成这个）；`from_sniff` 为嗅探页里找到的流（关掉时回到嗅探页），否则为粘贴、
    /// 启动参数或转交来的。
    pub(super) fn open_new_task(
        &mut self,
        link: Link,
        from_sniff: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (service, folder, home) =
            (self.service.clone(), self.folder.clone(), self.home.clone());
        let view = cx.new(|cx| NewTask::new(service, link, from_sniff, folder, home, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this: &mut Self, view, event: &NewTaskEvent, window, cx| {
                match event {
                    NewTaskEvent::Create(draft) => {
                        this.create(view.clone(), (**draft).clone(), window, cx)
                    }
                    NewTaskEvent::Dismissed => {
                        let back = match (&this.page, &this.sniff) {
                            (
                                Page::NewTask {
                                    from_sniff: true, ..
                                },
                                Some((sniff, _)),
                            ) => Page::Sniff(sniff.clone()),
                            _ => Page::Task,
                        };
                        this.show(back, cx);
                    }
                    NewTaskEvent::Ready => this.focus_new_task(view, window, cx),
                    NewTaskEvent::NotAPlaylist => this.check_page(view, window, cx),
                    NewTaskEvent::Sniff(url) => this.open_sniff(url.clone(), window, cx),
                    NewTaskEvent::FolderChanged(folder) => {
                        this.folder = folder.clone();
                        this.watch_free_space(cx);
                    }
                }
                cx.notify();
            },
        );
        let ready = view.read(cx).phase() == Phase::Ready;
        self.show(
            Page::NewTask {
                view: view.clone(),
                _subscription: subscription,
                from_sniff,
                checking: None,
            },
            cx,
        );
        if ready {
            self.focus_new_task(&view, window, cx);
        }
    }

    /// 新建页探测好了：把键盘焦点给它；用户还在粘贴栏里打别的链接时不抢，只在粘贴栏里还是这个链接、或焦点不在粘贴栏
    /// 时给。
    fn focus_new_task(
        &mut self,
        view: &Entity<NewTask>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let typing = self.paste.read(cx).focus_handle(cx).is_focused(window);
        let same = links::http_url(&self.paste.read(cx).value())
            .is_some_and(|url| &url == view.read(cx).url());
        if !typing || same {
            view.update(cx, |view, cx| view.focus(window, cx));
        }
    }

    /// 新建页 `view` 第一次探测取到的不是播放列表：粘贴来的、地址也不像播放列表的，在后台的网页里打开它，网页引擎
    /// 认出是网页就换到嗅探页找视频流（见 [`Shell::checked_page`]）；从嗅探页来的流、像播放列表的地址照常显示失败。
    fn check_page(&mut self, view: &Entity<NewTask>, window: &mut Window, cx: &mut Context<Self>) {
        let url = view.read(cx).url().clone();
        let Page::NewTask {
            view: shown,
            from_sniff: false,
            checking,
            ..
        } = &mut self.page
        else {
            return;
        };
        if shown != view || links::stream_url(url.as_str()).is_some() {
            return;
        }
        let service = self.service.clone();
        let sniff = cx.new(|cx| SniffView::new(service, url, window, cx));
        let subscription = cx.subscribe_in(
            &sniff,
            window,
            |this: &mut Self, sniff, event: &SniffEvent, window, cx| {
                if let SniffEvent::Decided = event {
                    this.checked_page(sniff, window, cx);
                }
            },
        );
        *checking = Some((sniff.clone(), subscription));
        view.update(cx, |view, cx| view.opening_page(cx));
        // 建不出网页时一开始就有结论
        self.checked_page(&sniff, window, cx);
    }

    /// 后台的网页 `sniff` 有了是不是网页的结论：是网页就换到嗅探页，否则在新建页的失败下面写明。新建页已换走、或用户
    /// 已改了请求头重新探测的，不再需要它。
    fn checked_page(
        &mut self,
        sniff: &Entity<SniffView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Page::NewTask { view, checking, .. } = &mut self.page else {
            return;
        };
        if checking
            .as_ref()
            .is_none_or(|(checked, _)| checked != sniff)
        {
            return;
        }
        if !view.read(cx).is_opening_page() {
            *checking = None;
            return;
        }
        let note = match sniff.read(cx).verdict() {
            Verdict::Pending => return,
            Verdict::Page => None,
            Verdict::NotPage(Some(kind)) => Some(format!("网页引擎打开它是 {kind}，不是网页")),
            Verdict::NotPage(None) => Some("网页引擎打开它也不是网页".to_owned()),
            Verdict::Broken(reason) => Some(format!("在网页里也打不开：{reason}")),
        };
        *checking = None;
        match note {
            Some(note) => view.update(cx, |view, cx| view.not_a_page(note, cx)),
            None => self.adopt_sniff(sniff.clone(), window, cx),
        }
        cx.notify();
    }

    /// 按新建视图里的选择新建任务；退出中不新建。新建要查磁盘，在服务的运行时里做，期间新建视图显示正在开始。
    pub(super) fn create(
        &mut self,
        view: Entity<NewTask>,
        draft: Draft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quitting() {
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
        if self.quitting() {
            self.service.pause(id);
        }
        if self.showing(view) {
            self.selected = Some(id);
            self.show(Page::Task, cx);
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
        if self.quitting() {
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
        self.new_task() == Some(view)
    }

    /// 在嗅探页里打开网页 `url`（已开着的换成这个）。
    fn open_sniff(&mut self, url: Url, window: &mut Window, cx: &mut Context<Self>) {
        let service = self.service.clone();
        let view = cx.new(|cx| SniffView::new(service, url, window, cx));
        self.adopt_sniff(view, window, cx);
    }

    /// 让 `view` 成为嗅探页（已开着的换成它）并显示。
    fn adopt_sniff(
        &mut self,
        view: Entity<SniffView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this: &mut Self, view, event: &SniffEvent, window, cx| {
                match event {
                    SniffEvent::Open(link) => this.open_new_task(link.clone(), true, window, cx),
                    SniffEvent::Dismissed => {
                        if matches!(&this.page, Page::Sniff(shown) if shown == view) {
                            this.show(Page::Task, cx);
                        }
                        this.sniff = None;
                    }
                    SniffEvent::PageToggled | SniffEvent::Decided => {}
                }
                cx.notify();
            },
        );
        self.sniff = Some((view.clone(), subscription));
        self.show(Page::Sniff(view), cx);
    }
}
