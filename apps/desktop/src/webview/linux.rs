//! Linux：把 wry 的 WebKitGTK 视图放进 GPUI 窗口的一个 X11 子窗口。GTK 嵌不进不归它管的 Wayland 表面，所以应用要跑在
//! X11（或 XWayland）上。源自 gpui-webview 0.9.1（Apache-2.0，见同目录 LICENSE-APACHE），只留不用窗口合成时用到的部分。

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui_kit::{
    App, Bounds, Global, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels,
    PlatformInput, Point, Window, point, px,
};
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, WindowHandle, XlibWindowHandle,
};

/// How often pending GTK events are dispatched between GPUI frames.
const GTK_POLL_INTERVAL: Duration = Duration::from_millis(16);

/// Marks that GTK is initialized and its main loop is driven by GPUI.
struct GtkMainLoop;

impl Global for GtkMainLoop {}

/// Initialize GTK on the X11 backend once and drive its main loop from GPUI.
pub(crate) fn ensure_gtk(cx: &mut App) -> wry::Result<()> {
    if cx.has_global::<GtkMainLoop>() {
        return Ok(());
    }

    if !gtk::is_initialized_main_thread() {
        // Wry downcasts the default GDK display to X11.
        gtk::gdk::set_allowed_backends("x11");
        gtk::init().map_err(|error| wry::Error::Io(std::io::Error::other(error)))?;
    }

    cx.set_global(GtkMainLoop);
    cx.spawn(async move |cx| {
        loop {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            cx.background_executor().timer(GTK_POLL_INTERVAL).await;
        }
    })
    .detach();

    Ok(())
}

/// The GPUI window as the Xlib parent Wry requires; GPUI reports its X11 window through XCB.
pub(crate) struct X11Parent(XlibWindowHandle);

impl X11Parent {
    pub(crate) fn new(window: &Window) -> wry::Result<Self> {
        Self::from_raw(HasWindowHandle::window_handle(window)?.as_raw())
    }

    fn from_raw(handle: RawWindowHandle) -> wry::Result<Self> {
        match handle {
            RawWindowHandle::Xcb(handle) => {
                let mut xlib = XlibWindowHandle::new(handle.window.get().into());
                xlib.visual_id = handle.visual_id.map_or(0, |id| id.get().into());
                Ok(Self(xlib))
            }
            RawWindowHandle::Xlib(handle) => Ok(Self(handle)),
            _ => Err(wry::Error::Io(std::io::Error::other(
                "WebView on Linux requires an X11 window; start the application with \
                 `gpui_kit::platform::linux(WindowingModes::X11)`",
            ))),
        }
    }
}

impl HasWindowHandle for X11Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: The X11 window outlives the webview, which is destroyed with the GPUI window.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Xlib(self.0)) })
    }
}

/// Wry scales bounds by GDK's factor, which ignores GPUI's, so pass device pixels.
pub(crate) fn device_bounds(bounds: Bounds<Pixels>, scale_factor: f32) -> wry::Rect {
    let device = |value: Pixels| f32::from(value) * scale_factor;
    wry::Rect {
        position: PhysicalPosition::new(device(bounds.origin.x), device(bounds.origin.y)).into(),
        size: PhysicalSize::new(device(bounds.size.width), device(bounds.size.height)).into(),
    }
}

/// Forward presses on the page to GPUI, which does not otherwise see them, so overlays that
/// close on an outside click (popovers, menus) close when the page is clicked.
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
            // GTK dispatches this from the GTK main loop; update GPUI once it is done.
            cx.spawn(async move |cx| {
                handle.update(cx, |_, window, cx| {
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
                })
            })
            .detach();
            Propagation::Proceed
        });
}

/// Zoom the page so its CSS pixels match GPUI's scale (GDK only supports integer scales).
pub(crate) fn match_scale_factor(webview: &wry::WebView, scale_factor: f32) -> wry::Result<()> {
    use gtk::prelude::WidgetExt as _;
    use wry::WebViewExtUnix as _;

    let gdk_scale = webview.webview().scale_factor().max(1);
    webview.zoom(f64::from(scale_factor) / f64::from(gdk_scale))
}
