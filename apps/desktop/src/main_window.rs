//! 主窗口与它背后的任务状态。[`Tasks`] 由这里持有到应用退出，主窗口关掉后任务照常进行；点托盘、程序坞或再次打开
//! 应用时重新打开窗口（或带到前面）。退出应用的请求（托盘、macOS 的程序坞与菜单、注销、关机）也从这里进来。

use gpui_kit::component::TitleBar;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, Global, TitlebarOptions, WeakEntity,
    WindowBounds, WindowOptions, point, px, size,
};
use hs_m3u8_instance::Handoff;

use crate::shell::{Leave, MIN_HEIGHT, MIN_WIDTH, Shell};
use crate::tasks::{Tasks, TasksEvent};
use crate::ui::TITLE_HEIGHT;

/// 任务状态与开着的主窗口（同时最多一个；Shell 已释放即窗口已关）。
struct MainWindow {
    tasks: Entity<Tasks>,
    open: Option<(AnyWindowHandle, WeakEntity<Shell>)>,
}

impl Global for MainWindow {}

/// 主窗口：标题栏透明，顶栏由应用自己画（红绿灯嵌在顶栏里）。
pub fn options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1440.), px(900.)),
            cx,
        ))),
        window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
        titlebar: Some(TitlebarOptions {
            title: Some("hs-m3u8".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(20.), px(TITLE_HEIGHT / 2. - 6.))),
        }),
        ..TitleBar::window_options()
    }
}

/// 接管 `tasks` 到应用退出，并把后打开的实例转交来的交给主窗口；之后才能开主窗口。
pub fn install(tasks: Entity<Tasks>, cx: &mut App) {
    cx.subscribe(&tasks, |_, event: &TasksEvent, cx| match event {
        TasksEvent::HandedOver(handoff) => hand_over(handoff.clone(), cx),
    })
    .detach();
    cx.set_global(MainWindow { tasks, open: None });
}

/// 任务状态；还没 [`install`] 时（启动失败）为 None。
pub fn tasks(cx: &App) -> Option<Entity<Tasks>> {
    Some(cx.try_global::<MainWindow>()?.tasks.clone())
}

/// 开着的主窗口与它的视图。
fn find(cx: &App) -> Option<(AnyWindowHandle, Entity<Shell>)> {
    let (handle, shell) = cx.try_global::<MainWindow>()?.open.as_ref()?;
    Some((*handle, shell.upgrade()?))
}

/// 开主窗口并带到前面；`link` 为要探测的链接（启动参数里给的），已开着时不用。还没 [`install`] 时为 None；开不了
/// 窗口（窗口系统出错）时为原因。
pub fn show(
    link: Option<String>,
    cx: &mut App,
) -> Option<gpui_kit::Result<(AnyWindowHandle, Entity<Shell>)>> {
    let tasks = tasks(cx)?;
    cx.activate(true);
    if let Some((handle, shell)) = find(cx) {
        return Some(
            handle
                .update(cx, |_, window, _| window.activate_window())
                .map(|()| (handle, shell)),
        );
    }
    let opened = gpui_kit::open_window(options(cx), cx, |window, cx| {
        cx.new(|cx| Shell::new(tasks, link, window, cx))
    });
    if let Ok((handle, shell)) = &opened {
        cx.global_mut::<MainWindow>().open = Some((*handle, shell.downgrade()));
    }
    Some(opened)
}

/// 点托盘或程序坞要回到窗口：开着时带到前面，关着时重新打开。开不了时提示原因（下次窗口打开时看得到）。
pub fn reopen(cx: &mut App) {
    if let Some(Err(error)) = show(None, cx) {
        tell(format!("开不了窗口：{error}"), cx);
    }
}

/// 再次打开应用时转交来的（已接手）：主窗口带到前面（关着时重新打开），有链接就在里面探测。开不了窗口时提示原因。
fn hand_over(handoff: Handoff, cx: &mut App) {
    let Some(shown) = show(None, cx) else {
        return;
    };
    let handed = shown.and_then(|(handle, shell)| {
        handle.update(cx, |_, window, cx| {
            shell.update(cx, |shell, cx| shell.handed_over(handoff, window, cx))
        })
    });
    if let Err(error) = handed {
        tell(format!("再次打开应用时开不了窗口：{error}"), cx);
    }
}

/// 加一条提示（窗口关着时，下次打开窗口时看得到）。
fn tell(text: String, cx: &mut App) {
    if let Some(tasks) = tasks(cx) {
        tasks.update(cx, |tasks, cx| {
            tasks.notify(text);
            cx.notify();
        });
    }
}

/// 要退出应用：不用问时（见 [`Tasks::quits_without_asking`]）为 true；要问时把主窗口带到前面（关着时重新打开）问，为
/// false，问完由 Tasks 退出或留下。开不了窗口、问不了时为 true：照常退出，任务在退出时暂停（见 `Tasks::new`）。
pub fn may_quit(cx: &mut App) -> bool {
    let Some(tasks) = tasks(cx) else {
        return true;
    };
    if tasks.read(cx).quits_without_asking() {
        return true;
    }
    let Some(Ok((handle, shell))) = show(None, cx) else {
        return true;
    };
    handle
        .update(cx, |_, window, cx| {
            shell.update(cx, |shell, cx| shell.should_leave(Leave::App, window, cx))
        })
        .unwrap_or(true)
}

/// 托盘里点了退出：不用问时立即退出，否则问（见 [`may_quit`]）。
pub fn quit(cx: &mut App) {
    if may_quit(cx)
        && let Some(tasks) = tasks(cx)
    {
        tasks.update(cx, |tasks, cx| tasks.leave(cx));
    }
}

/// macOS 的程序坞、菜单、注销、关机要退出时先问（见 [`may_quit`]）：要问时先不答复，问完、等完由
/// [`Tasks::leave`] 或 [`Tasks::stay`] 答复，注销、关机随之接着进行或取消。启动时没装上询问的方法时为原因。
#[cfg(target_os = "macos")]
pub fn ask_before_terminate(cx: &mut App) -> Result<(), hs_m3u8_macos::Error> {
    let app = cx.to_async();
    // 系统在主线程的事件循环里调，不在 GPUI 持有应用状态时：GPUI 的退出是异步发出的
    hs_m3u8_macos::ask_before_terminate(move || {
        app.update(|cx| {
            if may_quit(cx) {
                hs_m3u8_macos::Reply::Now
            } else {
                hs_m3u8_macos::Reply::Later
            }
        })
    })
}
