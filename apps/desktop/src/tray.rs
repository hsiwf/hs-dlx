//! 托盘图标（macOS 菜单栏、Windows 通知区域、Linux 的 StatusNotifierItem）：一个环，空闲时是应用的标志，在下点播时
//! 按它们的平均进度走，在录直播时中间加一个实心点；macOS 在图标旁写百分比与 REC，别处写进悬停提示。菜单里是进行中的
//! 数量与速度、待看的提示、打开窗口与退出；Windows、Linux 上左键点图标即打开窗口。
//!
//! Linux 上桌面没有托盘宿主（如没装扩展的 GNOME）时图标建得起却看不见，所以它不是回到窗口的唯一入口：再次打开应用
//! 也会回到窗口（见 hs-m3u8-instance）。

use std::time::{Duration, Instant};

use gpui_kit::{App, AppContext as _, Context, Entity, Global, Rgba, Subscription, Task};
use tokio::sync::mpsc;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::tasks::Tasks;
use crate::theme;

/// 两次刷新托盘之间至少隔多久：进度每秒变好几次，菜单栏重画与 D-Bus 往返都不必跟这么快。
const REFRESH: Duration = Duration::from_secs(1);
/// 空闲时的环画到哪里：与顶栏的标志相同。
const LOGO_ARC: f32 = 0.72;
/// 图标的边长（像素）：macOS 菜单栏高 22 点，按 2 倍屏画；别处系统再缩放。
#[cfg(target_os = "macos")]
const ICON: u32 = 44;
#[cfg(not(target_os = "macos"))]
const ICON: u32 = 32;

/// 托盘上要用户做的事。
enum Action {
    Open,
    Quit,
}

/// 托盘此刻显示的内容；与上次相同的部分不再推给系统。
#[derive(Clone, PartialEq)]
struct Shown {
    /// 环画到哪里，按百分之一取整；None 为空闲时的标志
    percent: Option<u8>,
    recording: bool,
    status: String,
    notices: usize,
}

/// 应用的托盘：建起来后一直留到应用退出，释放即从系统里移除。
struct Installed {
    _tray: Entity<Tray>,
}

impl Global for Installed {}

pub struct Tray {
    icon: TrayIcon,
    menu: Menu,
    status: MenuItem,
    /// 「N 条提示待看」：有提示时插在状态那一行下面
    notices: MenuItem,
    shown: Option<Shown>,
    refreshed_at: Option<Instant>,
    /// 还没到刷新间隔时，到点再刷新的那一次
    pending: Option<Task<()>>,
    /// 托盘出过错：之后不再刷新（原因已提示一次）
    broken: bool,
    _actions: Task<()>,
    _tasks: Subscription,
}

impl Tray {
    /// 建托盘图标并留到应用退出，跟着 `tasks` 刷新；点「打开」时调 `open`，点「退出」时调 `quit`。要在应用跑起来之后
    /// 调（GPUI 在主线程上调 `run` 的回调）。建不起（如 Linux 上没有 D-Bus 会话）时为原因。
    pub fn install(
        tasks: Entity<Tasks>,
        open: impl Fn(&mut App) + 'static,
        quit: impl Fn(&mut App) + 'static,
        cx: &mut App,
    ) -> Result<(), String> {
        let status = MenuItem::new("", false, None);
        let notices = MenuItem::new("", false, None);
        let opening = MenuItem::new("打开 hs-m3u8", true, None);
        let quitting = MenuItem::new("退出 hs-m3u8", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &opening,
            &PredefinedMenuItem::separator(),
            &quitting,
        ])
        .map_err(|error| format!("建不起托盘菜单：{error}"))?;
        let logo = ring_icon(LOGO_ARC, false)?;
        // macOS 上是模板图：只看透明度，系统按菜单栏的深浅配色
        #[cfg(target_os = "macos")]
        let builder = TrayIconBuilder::new().with_icon_templated(logo);
        #[cfg(not(target_os = "macos"))]
        let builder = TrayIconBuilder::new().with_icon(logo);
        let icon = builder
            .with_menu(Box::new(menu.clone()))
            .with_tooltip("hs-m3u8")
            // macOS 上点图标照例弹菜单；别处左键打开窗口、右键弹菜单
            .with_menu_on_left_click(cfg!(target_os = "macos"))
            .build()
            .map_err(|error| format!("建不起托盘图标：{error}"))?;

        // 系统在自己的线程或回调里报点击：转进通道，在界面线程上处理。通道随应用存在，发送不会失败；收的一方先
        // 不在时（应用正在退出）丢掉即可
        let (sender, mut actions) = mpsc::unbounded_channel();
        let (open_id, quit_id) = (opening.id().clone(), quitting.id().clone());
        let menu_sender = sender.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = if event.id == open_id {
                Action::Open
            } else if event.id == quit_id {
                Action::Quit
            } else {
                return;
            };
            let _closing = menu_sender.send(action);
        }));
        if !cfg!(target_os = "macos") {
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    let _closing = sender.send(Action::Open);
                }
            }));
        }
        let tray = cx.new(|cx| {
            let handling = cx.spawn(async move |_, cx| {
                while let Some(action) = actions.recv().await {
                    cx.update(|cx| match action {
                        Action::Open => open(cx),
                        Action::Quit => quit(cx),
                    });
                }
            });
            let observing =
                cx.observe(&tasks, |tray: &mut Self, tasks, cx| tray.changed(tasks, cx));
            let mut tray = Tray {
                icon,
                menu,
                status,
                notices,
                shown: None,
                refreshed_at: None,
                pending: None,
                broken: false,
                _actions: handling,
                _tasks: observing,
            };
            tray.refresh(&tasks, cx);
            tray
        });
        cx.set_global(Installed { _tray: tray });
        Ok(())
    }

    /// 任务有变化：离上次刷新已过了间隔就立即刷新，否则到点再刷新一次。
    fn changed(&mut self, tasks: Entity<Tasks>, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            return;
        }
        let wait = self
            .refreshed_at
            .map_or(Duration::ZERO, |at| REFRESH.saturating_sub(at.elapsed()));
        if wait.is_zero() {
            self.refresh(&tasks, cx);
            return;
        }
        self.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            // 托盘已随应用释放时不再刷新
            let _gone = this.update(cx, |tray, cx| {
                tray.pending = None;
                tray.refresh(&tasks, cx);
            });
        }));
    }

    /// 按此刻的任务刷新图标、标题、提示与菜单；出错时提示一次，之后不再刷新。
    fn refresh(&mut self, tasks: &Entity<Tasks>, cx: &mut Context<Self>) {
        if self.broken {
            return;
        }
        self.refreshed_at = Some(Instant::now());
        let read = tasks.read(cx);
        let activity = read.activity(Instant::now());
        let shown = Shown {
            percent: activity
                .progress
                .map(|progress| (progress.clamp(0., 1.) * 100.).round() as u8),
            recording: activity.recording,
            status: activity
                .summary()
                .unwrap_or_else(|| "没有进行中的任务".to_owned()),
            notices: read.notices().len(),
        };
        if let Err(error) = self.apply(&shown) {
            self.broken = true;
            tasks.update(cx, |tasks, cx| {
                tasks.tell(format!("托盘图标出错，之后不再更新：{error}"), cx)
            });
            return;
        }
        self.shown = Some(shown);
    }

    /// 把 `shown` 与上次不同的部分推给系统。
    fn apply(&self, shown: &Shown) -> Result<(), String> {
        let was = self.shown.as_ref();
        if was.is_none_or(|was| (was.percent, was.recording) != (shown.percent, shown.recording)) {
            let arc = shown
                .percent
                .map_or(LOGO_ARC, |percent| f32::from(percent) / 100.);
            let icon = ring_icon(arc, shown.recording)?;
            #[cfg(target_os = "macos")]
            let set = self.icon.set_icon_templated(Some(icon));
            #[cfg(not(target_os = "macos"))]
            let set = self.icon.set_icon(Some(icon));
            set.map_err(|error| error.to_string())?;
            // 只 macOS 在图标旁显示标题
            let title = match (shown.percent, shown.recording) {
                (Some(percent), false) => Some(format!("{percent}%")),
                (Some(percent), true) => Some(format!("{percent}% · REC")),
                (None, true) => Some("REC".to_owned()),
                (None, false) => None,
            };
            self.icon.set_title(title);
        }
        if was.is_none_or(|was| was.status != shown.status) {
            self.status.set_text(&shown.status);
            self.icon
                .set_tooltip(Some(format!("hs-m3u8 · {}", shown.status)))
                .map_err(|error| error.to_string())?;
        }
        let had = was.map_or(0, |was| was.notices);
        if had != shown.notices {
            self.notices
                .set_text(format!("{} 条提示待看", shown.notices));
            match (had, shown.notices) {
                (0, _) => self
                    .menu
                    .insert(&self.notices, 1)
                    .map_err(|error| error.to_string())?,
                (_, 0) => self
                    .menu
                    .remove(&self.notices)
                    .map_err(|error| error.to_string())?,
                _ => {}
            }
        }
        Ok(())
    }
}

/// 托盘的环：从正上方顺时针画到 `arc`（0 到 1），其余是淡的底环；`dot` 时中间加一个实心点。macOS 上是模板图（只看
/// 透明度，系统按菜单栏配色），别处是应用的青色，点为录制的红色。
fn ring_icon(arc: f32, dot: bool) -> Result<Icon, String> {
    let (ink, track, mark) = if cfg!(target_os = "macos") {
        ([0, 0, 0, 255], [0, 0, 0, 90], [0, 0, 0, 255])
    } else {
        (
            rgba8(theme::cyan()),
            [140, 147, 166, 150],
            rgba8(theme::red()),
        )
    };
    Icon::from_rgba(ring(ICON, arc, dot, ink, track, mark), ICON, ICON)
        .map_err(|error| format!("画不出托盘图标：{error}"))
}

fn rgba8(color: gpui_kit::Hsla) -> [u8; 4] {
    let rgba = Rgba::from(color);
    [rgba.r, rgba.g, rgba.b, rgba.a].map(|channel| (channel.clamp(0., 1.) * 255.).round() as u8)
}

/// 画边长 `size` 像素的环，返回逐行的 RGBA（不预乘透明度）。环的中线半径为边长的 0.34、宽为 0.09，点的半径为
/// 0.1；边缘按像素被覆盖的比例抗锯齿。
fn ring(size: u32, arc: f32, dot: bool, ink: [u8; 4], track: [u8; 4], mark: [u8; 4]) -> Vec<u8> {
    let side = size as f32;
    let (center, radius, half_width, dot_radius) =
        (side / 2., side * 0.34, side * 0.045, side * 0.1);
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 + 0.5 - center, y as f32 + 0.5 - center);
            let distance = dx.hypot(dy);
            let band = (half_width + 0.5 - (distance - radius).abs()).clamp(0., 1.);
            // 从正上方起顺时针转过的比例（0 到 1）
            let turned = dx.atan2(-dy).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
            let color = if turned <= arc { ink } else { track };
            let mut alpha = f32::from(color[3]) * band;
            let mut rgb = [color[0], color[1], color[2]];
            if dot {
                let inside = (dot_radius + 0.5 - distance).clamp(0., 1.);
                if inside > 0. {
                    alpha = f32::from(mark[3]) * inside;
                    rgb = [mark[0], mark[1], mark[2]];
                }
            }
            pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], alpha.round() as u8]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::ring;

    const INK: [u8; 4] = [0, 0, 255, 255];
    const TRACK: [u8; 4] = [128, 128, 128, 100];
    const MARK: [u8; 4] = [255, 0, 0, 255];

    /// 边长 44 时第 `x`、`y` 个像素。
    fn pixel(pixels: &[u8], x: usize, y: usize) -> [u8; 4] {
        let at = (y * 44 + x) * 4;
        pixels[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn the_ring_fills_clockwise_from_the_top() {
        // 中线半径 44 × 0.34 ≈ 15：正上方、正右、正下、正左各取环上的一个像素
        let quarter = ring(44, 0.3, false, INK, TRACK, MARK);
        assert_eq!(pixel(&quarter, 22, 7), INK, "正上方是起点");
        assert_eq!(pixel(&quarter, 36, 22), INK, "四分之一圈已画到");
        assert_eq!(pixel(&quarter, 22, 36), TRACK, "半圈还没到");
        assert_eq!(pixel(&quarter, 7, 22), TRACK);
        // 环外与环心透明
        assert_eq!(pixel(&quarter, 0, 0)[3], 0);
        assert_eq!(pixel(&quarter, 22, 22)[3], 0);
        // 录制时环心是实心点
        let recording = ring(44, 1., true, INK, TRACK, MARK);
        assert_eq!(pixel(&recording, 22, 22), MARK);
        assert_eq!(pixel(&recording, 7, 22), INK, "满圈");
    }
}
