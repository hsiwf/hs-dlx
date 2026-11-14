//! 主窗口：顶栏；左边一列任务小环；中间是选中任务的刻度环（新建时换成同心环，没有任务时是待机环）；右边是这个任务
//! 的信号；底部是粘贴栏；右上角是要用户关掉的提示。任务的状态在 [`crate::tasks`] 里，这里管这个窗口自己的状态与
//! 事件；新建、对任务的操作、退出时的询问与各块的画法在子模块里。

mod creating;
mod live_view;
mod notices;
mod operations;
mod paste_bar;
mod plan;
mod quit;
mod task_list;
mod task_view;
mod title_bar;

use std::path::PathBuf;
use std::time::Instant;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Window, div, px,
};
use hs_m3u8_app::TaskId;
use hs_m3u8_instance::Handoff;

use crate::headers::HeadersEditor;
use crate::links;
use crate::new_task;
use crate::new_task::NewTask;
use crate::settings::{SettingsEvent, SettingsView};
use crate::sniff::SniffView;
use crate::tasks::{Row, Tasks, TasksEvent};
use crate::theme;
use crate::ui::{GAP, PAD, TITLE_HEIGHT, max};

/// 窗口的最小宽：放得下列表收到最窄时、最宽的一个视图（待机页没有列表，比它们窄）。
pub const MIN_WIDTH: f32 =
    2. * PAD + task_list::MIN_WIDTH + GAP + max(new_task::MIN_WIDTH, task_view::MIN_WIDTH);
/// 窗口的最小高：放得下顶栏、最高的一个视图与粘贴栏。
pub const MIN_HEIGHT: f32 = TITLE_HEIGHT
    + max(
        max(task_view::MIN_HEIGHT, task_view::STANDBY_MIN_HEIGHT),
        new_task::MIN_HEIGHT,
    )
    + paste_bar::HEIGHT;

/// 主体显示的页。
enum Page {
    /// 选中的任务；没有任务时为待机页
    Task,
    /// 新建页：视图与对它的订阅，离开新建页时一起释放；`from_sniff` 为从嗅探页打开的，关掉时回到嗅探页；
    /// `page_check` 为第一次探测取到的不是播放列表时，在后台打开它、看是不是网页的嗅探视图与对它的订阅
    NewTask {
        view: Entity<NewTask>,
        _subscription: Subscription,
        from_sniff: bool,
        page_check: Option<(Entity<SniffView>, Subscription)>,
    },
    /// 设置页：视图与对它的订阅，离开时一起释放
    Settings {
        view: Entity<SettingsView>,
        _subscription: Subscription,
    },
    /// 网页里的视频流：只经 [`Shell::show_sniff`] 换上，视图即 [`Shell::sniff`] 里的；网页显示着时占满主体，不显示任务
    /// 列表
    Sniff(Entity<SniffView>),
}

/// 主窗口的视图：任务状态在 [`Tasks`] 里，这里是这个窗口自己的（选中哪个任务、显示哪一页、粘贴栏、嗅探页）。
pub struct Shell {
    tasks: Entity<Tasks>,
    /// 选中的任务；不在列表里（已移除）或没选过时为最新的一个，见 [`Shell::selected_row`]
    selected: Option<TaskId>,
    /// 主体显示的页，只经 [`Shell::show`] 换
    page: Page,
    /// 嗅探页与对它的订阅：关掉之前一直留着（不显示时网页藏起来、不出声），任务列表里有一项可以回去
    sniff: Option<(Entity<SniffView>, Subscription)>,
    paste: Entity<InputState>,
    home: Option<PathBuf>,
    /// 粘贴栏里的内容不能探测的原因；再探测或改了内容时清掉
    input_error: Option<String>,
    /// 剪贴板里一条还没下过、也不在探测中的 m3u8 链接
    clipboard: Option<hs_m3u8_core::Url>,
    /// 点过一次「放弃任务」、等着再点一次确认的任务
    confirm_discard: Option<TaskId>,
    /// 正在改请求头的停着的任务与编辑框
    editing_headers: Option<(TaskId, Entity<HeadersEditor>)>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    /// `link` 为启动参数里给的链接：有就填进粘贴栏并探测。
    pub fn new(
        tasks: Entity<Tasks>,
        link: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let paste = cx.new(|cx| InputState::new(window, cx).placeholder("粘贴链接，回车探测"));
        paste.update(cx, |input, cx| input.focus(window, cx));
        let typed = cx.subscribe_in(
            &paste,
            window,
            |this: &mut Self, input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let text = input.read(cx).value().to_string();
                    this.probe(&text, window, cx);
                }
                InputEvent::Change => {
                    this.input_error = None;
                    cx.notify();
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        let activated = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.refresh_clipboard(cx);
            }
        });
        let changed = cx.observe(&tasks, |_, _, cx| cx.notify());
        let handed = cx.subscribe_in(
            &tasks,
            window,
            |this: &mut Self, _, event: &TasksEvent, window, cx| match event {
                TasksEvent::HandedOver(handoff) => this.handed_over(handoff.clone(), window, cx),
            },
        );
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            // 视图已不在时没有要保护的任务
            this.update(cx, |shell, cx| shell.should_close(window, cx))
                .unwrap_or(true)
        });
        #[cfg(target_os = "macos")]
        if let Err(error) = quit::ask_before_terminate(window, cx) {
            tasks.update(cx, |tasks, _| {
                tasks.notify(format!("从程序坞退出、注销时不会先问：{error}"))
            });
        }
        let mut shell = Shell {
            tasks,
            selected: None,
            page: Page::Task,
            sniff: None,
            paste,
            home: dirs::home_dir(),
            input_error: None,
            clipboard: None,
            confirm_discard: None,
            editing_headers: None,
            _subscriptions: vec![typed, activated, changed, handed],
        };
        shell.refresh_clipboard(cx);
        if let Some(link) = link {
            shell.probe_text(&link, window, cx);
        }
        shell
    }

    /// 再次打开应用时转交来的（已接手）：窗口到前台；有链接就探测，新建页开着时不替换它，只填进粘贴栏。
    fn handed_over(&mut self, handoff: Handoff, window: &mut Window, cx: &mut Context<Self>) {
        cx.activate(true);
        window.activate_window();
        if let Handoff::Link(link) = handoff {
            if self.new_task().is_some() {
                self.paste
                    .update(cx, |input, cx| input.set_value(link, window, cx));
                self.notify(
                    "再次打开应用时转交的链接已填进粘贴栏：新建页还开着，回车即换成探测它"
                        .to_owned(),
                    cx,
                );
            } else {
                self.probe_text(&link, window, cx);
            }
        }
        cx.notify();
    }

    /// 把 `text` 填进粘贴栏并探测。
    pub(super) fn probe_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.paste
            .update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
        self.probe(text, window, cx);
    }

    /// 选中的一行：选过的还在列表里时为它，否则为最新的一行；没有任务时为 None。
    fn selected_row<'a>(&self, tasks: &'a Tasks) -> Option<&'a Row> {
        self.selected
            .and_then(|id| tasks.row(id))
            .or_else(|| tasks.rows().last())
    }

    /// 加一条要用户关掉的提示。
    fn notify(&mut self, text: String, cx: &mut Context<Self>) {
        self.tasks.update(cx, |tasks, cx| {
            tasks.notify(text);
            cx.notify();
        });
    }

    /// 重读剪贴板：是 http(s) 的 m3u8 链接、不是已有任务也不在探测中时记下，供一键探测。
    fn refresh_clipboard(&mut self, cx: &mut Context<Self>) {
        let found = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .and_then(|text| links::stream_url(&text));
        let probing = self.new_task().map(|view| view.read(cx).url().clone());
        let tasks = self.tasks.read(cx);
        self.clipboard = found.filter(|url| {
            !tasks.rows().iter().any(|row| &row.task.spec.url == url)
                && probing.as_ref() != Some(url)
        });
        cx.notify();
    }

    fn select(&mut self, id: TaskId, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.confirm_discard = None;
        self.editing_headers = None;
        self.show(Page::Task, cx);
    }

    /// 换到 `page`（原来的新建页随之释放）；告诉离开与换上的嗅探视图（见 [`SniffView::set_on_screen`]）。
    fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        let sniff_of = |page: &Page| match page {
            Page::Sniff(view) => Some(view.clone()),
            Page::Task | Page::NewTask { .. } | Page::Settings { .. } => None,
        };
        let (was, will) = (sniff_of(&self.page), sniff_of(&page));
        if was != will {
            if let Some(was) = was {
                was.update(cx, |sniff, cx| sniff.set_on_screen(false, cx));
            }
            if let Some(will) = will {
                will.update(cx, |sniff, cx| sniff.set_on_screen(true, cx));
            }
        }
        self.page = page;
        cx.notify();
    }

    /// 打开设置页；已开着时关上。读不出设置时提示原因。
    fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.page, Page::Settings { .. }) {
            self.show(Page::Task, cx);
            return;
        }
        let tasks = self.tasks.read(cx);
        let (service, folder) = (tasks.service().clone(), tasks.folder().clone());
        let proxy = match service.settings() {
            Ok(settings) => settings.proxy,
            Err(error) => {
                self.notify(format!("读不出设置：{error}"), cx);
                return;
            }
        };
        let home = self.home.clone();
        let view = cx.new(|cx| SettingsView::new(service, folder, home, proxy, window, cx));
        let subscription = cx.subscribe(&view, |this: &mut Self, _, event: &SettingsEvent, cx| {
            match event {
                SettingsEvent::Dismissed => this.show(Page::Task, cx),
                SettingsEvent::FolderChanged(folder) => this
                    .tasks
                    .update(cx, |tasks, cx| tasks.set_folder(folder.clone(), cx)),
            }
            cx.notify();
        });
        self.show(
            Page::Settings {
                view,
                _subscription: subscription,
            },
            cx,
        );
    }

    /// 换到嗅探页（[`Shell::sniff`] 里的视图）；嗅探页已关时什么也不做（旧帧上的点击）。
    fn show_sniff(&mut self, cx: &mut Context<Self>) {
        if let Some((view, _)) = &self.sniff {
            self.show(Page::Sniff(view.clone()), cx);
        }
    }

    /// 显示着的新建视图；不在新建页时为 None。
    fn new_task(&self) -> Option<&Entity<NewTask>> {
        match &self.page {
            Page::NewTask { view, .. } => Some(view),
            Page::Task | Page::Sniff(_) | Page::Settings { .. } => None,
        }
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let cx = &*cx;
        let tasks = self.tasks.read(cx);
        let mut body = div().flex_1().min_h_0().flex().gap(px(GAP)).px(px(PAD));
        let listed = !tasks.rows().is_empty() || self.new_task().is_some() || self.sniff.is_some();
        // 网页显示着时占满主体，不显示任务列表
        let page_shown = matches!(&self.page, Page::Sniff(sniff) if sniff.read(cx).page_shown());
        if !page_shown && listed {
            body = body.child(self.render_list(tasks, now, cx));
        }
        body = match &self.page {
            Page::NewTask { view, .. } => body.child(view.clone()),
            Page::Sniff(view) => body.child(view.clone()),
            Page::Settings { view, .. } => body.child(view.clone()),
            Page::Task => match self.selected_row(tasks) {
                Some(row) => {
                    let (main, aside) = self.render_task(row, now, cx);
                    body.child(main).child(aside)
                }
                None => body.child(self.render_standby(cx)),
            },
        };
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::ground())
            .text_color(theme::text())
            .text_size(px(14.))
            .child(self.render_header(tasks, now, cx))
            .child(body)
            .child(self.render_footer(tasks, cx))
            .child(self.render_notices(tasks, cx))
    }
}
