//! Linux：把 wry 的 WebKitGTK 视图放进 GPUI 窗口的一个 X11 子窗口。GTK 嵌不进不归它管的 Wayland 表面，所以窗口要是
//! X11 的（Wayland 会话里由 XWayland 提供）。源自 gpui-webview 0.9.1（Apache-2.0，见本 crate 的 LICENSE-APACHE），只留
//! 不用窗口合成时用到的部分。

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{
    App, Bounds, Global, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels,
    PlatformInput, Point, Window, point, px,
};
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, WindowHandle, XlibWindowHandle,
};

/// GPUI 多久处理一次积下的 GTK 事件：约一帧。
const GTK_POLL_INTERVAL: Duration = Duration::from_millis(16);
/// 一次最多处理多少个 GTK 事件：GLib 一直有就绪的事件源时，不让界面线程卡在这里。
const GTK_EVENTS_PER_POLL: usize = 256;

/// GTK 已初始化；由 GPUI 驱动它的主循环。
struct GtkLoop {
    /// 还在的 [`GtkUser`] 个数
    users: Rc<Cell<usize>>,
    /// 驱动主循环的任务在跑：最后一个使用者释放后它在下一次处理时停下
    running: Rc<Cell<bool>>,
}

impl Global for GtkLoop {}

/// GTK 主循环的一个使用者：有使用者期间 GPUI 约每帧处理一次积下的 GTK 事件（网页的绘制、输入与回调都靠它），最后一个
/// 使用者释放后停下，再有使用者时重新开始。
pub(crate) struct GtkUser {
    users: Rc<Cell<usize>>,
}

impl GtkUser {
    /// 第一次用时在 X11 后端上初始化 GTK（wry 要把默认的 GDK 显示当作 X11 的）。
    pub(crate) fn new(cx: &mut App) -> wry::Result<GtkUser> {
        if !cx.has_global::<GtkLoop>() {
            if !gtk::is_initialized_main_thread() {
                gtk::gdk::set_allowed_backends("x11");
                gtk::init().map_err(|error| wry::Error::Io(std::io::Error::other(error)))?;
            }
            cx.set_global(GtkLoop {
                users: Rc::new(Cell::new(0)),
                running: Rc::new(Cell::new(false)),
            });
        }
        let state = cx.global::<GtkLoop>();
        let (users, running) = (state.users.clone(), state.running.clone());
        users.set(users.get() + 1);
        if !running.replace(true) {
            let still = users.clone();
            cx.spawn(async move |cx| {
                loop {
                    for _ in 0..GTK_EVENTS_PER_POLL {
                        if !gtk::events_pending() {
                            break;
                        }
                        gtk::main_iteration_do(false);
                    }
                    if still.get() == 0 {
                        running.set(false);
                        break;
                    }
                    cx.background_executor().timer(GTK_POLL_INTERVAL).await;
                }
            })
            .detach();
        }
        Ok(GtkUser { users })
    }
}

impl Drop for GtkUser {
    fn drop(&mut self) {
        self.users.set(self.users.get() - 1);
    }
}

/// wry 要的 Xlib 父窗口：GPUI 经 XCB 给出它的 X11 窗口，窗口号相同。
pub(crate) struct X11Parent(XlibWindowHandle);

impl X11Parent {
    pub(crate) fn new(window: &Window) -> wry::Result<Self> {
        match HasWindowHandle::window_handle(window)?.as_raw() {
            RawWindowHandle::Xcb(handle) => {
                let mut xlib = XlibWindowHandle::new(handle.window.get().into());
                xlib.visual_id = handle.visual_id.map_or(0, |id| id.get().into());
                Ok(Self(xlib))
            }
            RawWindowHandle::Xlib(handle) => Ok(Self(handle)),
            _ => Err(wry::Error::Io(std::io::Error::other(
                "网页要放在 X11 窗口里，这个窗口不是：这个会话没有 X11（Wayland 会话里由 XWayland 提供）",
            ))),
        }
    }
}

impl HasWindowHandle for X11Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: 句柄指向 GPUI 窗口本身的 X11 窗口；网页视图建在它里面，随窗口一起销毁，用不到窗口之后。
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Xlib(self.0)) })
    }
}

/// wry 按 GDK 的缩放倍数换算位置与大小，不知道 GPUI 的倍数，所以直接给设备像素。
pub(crate) fn device_bounds(bounds: Bounds<Pixels>, scale_factor: f32) -> wry::Rect {
    let device = |value: Pixels| f32::from(value) * scale_factor;
    wry::Rect {
        position: PhysicalPosition::new(device(bounds.origin.x), device(bounds.origin.y)).into(),
        size: PhysicalSize::new(device(bounds.size.width), device(bounds.size.height)).into(),
    }
}

/// 把网页上的按下转给 GPUI（它本来看不到），点到网页时，点外面就关的弹层、菜单也关上。
pub(crate) fn forward_mouse_down(
    webview: &wry::WebView,
    origin: Rc<Cell<Point<Pixels>>>,
    window: &Window,
    cx: &App,
) {
    use gtk::{gdk::EventType, glib::Propagation, prelude::WidgetExt as _};
    use wry::WebViewExtUnix as _;

    let handle = window.window_handle();
    let cx = cx.to_async();
    webview
        .webview()
        .connect_button_press_event(move |widget, event| {
            let button = match event.button() {
                1 => MouseButton::Left,
                2 => MouseButton::Middle,
                3 => MouseButton::Right,
                _ => return Propagation::Proceed,
            };
            if event.event_type() != EventType::ButtonPress {
                return Propagation::Proceed;
            }
            let (x, y) = event.position();
            let gdk_scale = widget.scale_factor() as f32;
            let origin = origin.get();
            // GTK 在它的主循环里调这里，GPUI 正处理别的事件；等这一轮处理完再交给 GPUI
            cx.spawn(async move |cx| {
                // 窗口在这之前关掉时，没有要关的弹层
                let _closed = handle.update(cx, |_, window, cx| {
                    let scale = gdk_scale / window.scale_factor();
                    let position = origin + point(px(x as f32 * scale), px(y as f32 * scale));
                    window.dispatch_event(
                        PlatformInput::MouseDown(MouseDownEvent {
                            button,
                            position,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                            first_mouse: false,
                        }),
                        cx,
                    );
                    window.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent {
                            button,
                            position,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                        }),
                        cx,
                    );
                });
            })
            .detach();
            Propagation::Proceed
        });
}

/// 缩放页面，让它的 CSS 像素对上 GPUI 的缩放（GDK 只支持整数倍）。
pub(crate) fn match_scale_factor(webview: &wry::WebView, scale_factor: f32) -> wry::Result<()> {
    use gtk::prelude::WidgetExt as _;
    use wry::WebViewExtUnix as _;

    let gdk_scale = webview.webview().scale_factor().max(1);
    webview.zoom(f64::from(scale_factor) / f64::from(gdk_scale))
}
