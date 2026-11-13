//! 把 wry 的原生网页视图放进 GPUI 窗口：随布局摆位置、在被裁掉时藏起来，挡住的区域不让下面的 GPUI 内容收到点击。
//! 源自 gpui-webview 0.9.1（© Longbridge，Apache-2.0，全文见同目录 LICENSE-APACHE），改为依赖官方 wry，只留不用窗口
//! 合成时用到的部分。
//!
//! 原生视图直接挂在窗口上：它盖住所在区域里的 GPUI 内容（弹层、提示也盖住），也不能被部分裁剪（容器只裁掉一部分时它
//! 照样占满原来的区域），整个移出可见区域时才藏起来。

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{
    App, Bounds, ContentMask, Element, ElementId, Entity, GlobalElementId, Hitbox, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, MouseDownEvent, ParentElement as _, Pixels, Render,
    Size, Style, Styled as _, Window, div,
};
use wry::Rect;

/// 放在 GPUI 窗口里的网页。
pub struct WebView {
    webview: Rc<wry::WebView>,
    /// 摆位置或交还焦点最近一次失败的原因；成功后清掉。放在这里由使用方显示：画的过程中报不出错误
    trouble: RefCell<Option<String>>,
    /// GPUI 的缩放倍数，页面的缩放按它对齐（GDK 只支持整数倍）
    #[cfg(target_os = "linux")]
    scale_factor: Cell<f32>,
    /// 页面所在区域的左上角（窗口坐标），把页面上的点击转给 GPUI 时用
    #[cfg(target_os = "linux")]
    origin: Rc<Cell<gpui_kit::Point<Pixels>>>,
}

impl Drop for WebView {
    /// 视图本身随最后一个引用释放；先藏起来，不让它在窗口里多留一帧。
    fn drop(&mut self) {
        // 释放时没有调用方能处理；藏不起来的视图随窗口一起销毁
        let _hidden = self.webview.set_visible(false);
    }
}

impl WebView {
    /// 按 `builder` 在 `window` 里建网页，做好各平台 wry 要的准备。Linux 上窗口要跑在 X11 或 XWayland 上（Wayland
    /// 窗口报错），并在这里初始化 GTK、由 GPUI 驱动它的主循环。
    pub fn build(
        builder: wry::WebViewBuilder,
        window: &mut Window,
        cx: &mut App,
    ) -> wry::Result<Self> {
        #[cfg(target_os = "linux")]
        let webview = {
            let parent = linux::X11Parent::new(window)?;
            linux::ensure_gtk(cx)?;
            builder.build_as_child(&parent)?
        };
        #[cfg(not(target_os = "linux"))]
        let webview = builder.build_as_child(&*window)?;
        // 第一次布局之前先不占地方
        webview.set_bounds(Rect::default())?;
        #[cfg(target_os = "linux")]
        let origin = Rc::new(Cell::new(gpui_kit::Point::default()));
        #[cfg(target_os = "linux")]
        {
            linux::forward_mouse_down(&webview, origin.clone(), window, cx);
            linux::match_scale_factor(&webview, window.scale_factor())?;
        }
        #[cfg(not(target_os = "linux"))]
        let _ = cx;
        Ok(WebView {
            webview: Rc::new(webview),
            trouble: RefCell::new(None),
            #[cfg(target_os = "linux")]
            scale_factor: Cell::new(window.scale_factor()),
            #[cfg(target_os = "linux")]
            origin,
        })
    }

    /// 底层的 wry 网页。
    pub fn raw(&self) -> &wry::WebView {
        &self.webview
    }

    /// 摆位置或交还焦点最近一次失败的原因。
    pub fn trouble(&self) -> Option<String> {
        self.trouble.borrow().clone()
    }

    /// 按页面区域 `document` 与继承下来的裁剪区 `clip` 摆放：完全在裁剪区外时藏起来（页面照旧留着）。
    fn place(&self, document: Bounds<Pixels>, clip: Bounds<Pixels>) -> wry::Result<bool> {
        if document.intersect(&clip).is_empty() {
            self.webview.set_visible(false)?;
            return Ok(false);
        }
        #[cfg(target_os = "linux")]
        let rect = linux::device_bounds(document, self.scale_factor.get());
        #[cfg(not(target_os = "linux"))]
        let rect = Rect {
            position: wry::dpi::LogicalPosition::new(
                f64::from(document.origin.x),
                f64::from(document.origin.y),
            )
            .into(),
            size: wry::dpi::LogicalSize::new(
                f64::from(document.size.width),
                f64::from(document.size.height),
            )
            .into(),
        };
        self.webview.set_bounds(rect)?;
        self.webview.set_visible(true)?;
        Ok(true)
    }

    fn record(&self, result: wry::Result<()>, what: &str) {
        *self.trouble.borrow_mut() = result.err().map(|error| format!("{what}：{error}"));
    }
}

impl Render for WebView {
    fn render(&mut self, _: &mut Window, cx: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        div().size_full().child(WebViewElement {
            view: cx.entity(),
            webview: self.webview.clone(),
        })
    }
}

/// 摆放原生网页视图的元素。
struct WebViewElement {
    view: Entity<WebView>,
    webview: Rc<wry::WebView>,
}

impl IntoElement for WebViewElement {
    type Element = WebViewElement;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for WebViewElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: Size::full(),
            flex_shrink: 1.,
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let view = self.view.read(cx);
        #[cfg(target_os = "linux")]
        {
            if view.scale_factor.replace(window.scale_factor()) != window.scale_factor() {
                let matched = linux::match_scale_factor(&view.webview, window.scale_factor());
                view.record(matched, "网页缩放没能对齐窗口");
            }
            view.origin.set(bounds.origin);
        }
        let clip = window.content_mask().bounds;
        match view.place(bounds, clip) {
            Ok(true) => {
                view.record(Ok(()), "");
                // 原生视图盖住这一块，下面的 GPUI 内容不该收到点击
                Some(window.insert_hitbox(bounds.intersect(&clip), HitboxBehavior::BlockMouse))
            }
            Ok(false) => None,
            Err(error) => {
                view.record(Err(error), "网页没能摆到位置上");
                None
            }
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let bounds = hitbox.as_ref().map_or(bounds, |hitbox| hitbox.bounds);
        let view = self.view.clone();
        let webview = self.webview.clone();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.on_mouse_event(move |event: &MouseDownEvent, _, _, cx| {
                // 点在网页外面时把键盘焦点交还窗口，输入框才收得到字
                if !bounds.contains(&event.position) {
                    let returned = webview.focus_parent();
                    view.read(cx).record(returned, "键盘焦点没能交还窗口");
                }
            });
        });
    }
}
