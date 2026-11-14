//! 设置页：新建任务默认保存到哪、请求经哪个代理、版本。改了即存进任务存储（见 [`hs_m3u8_app::Settings`]）。

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Div, Entity, EventEmitter, FontWeight, IntoElement,
    ParentElement as _, PathPromptOptions, Render, Styled as _, Subscription, Task, Window, div,
    px,
};
use hs_m3u8_app::Service;
use hs_m3u8_core::{Proxy, Url};

use crate::route::Route;
use crate::theme;
use crate::ui::{self, BOTTOM, Tone};

/// 设置一栏的最大宽度。
const MAX_WIDTH: f32 = 640.;
/// 指定代理时输入框里的示例。
const PROXY_EXAMPLE: &str = "socks5://127.0.0.1:1080";

/// 发给外面的事件。
pub enum SettingsEvent {
    Dismissed,
    /// 换了保存位置（已存进设置）：新建任务默认存到这里
    FolderChanged(PathBuf),
}

/// 代理的三种方式；选中「指定代理」后要填了地址、保存才生效。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    System,
    Direct,
    Custom,
}

pub struct SettingsView {
    service: Rc<Service>,
    folder: PathBuf,
    home: Option<PathBuf>,
    /// 存着的代理
    proxy: Proxy,
    /// 选中的方式：选了「指定代理」而还没保存地址时与 `proxy` 不同
    mode: Mode,
    address: Entity<InputState>,
    /// 跟随系统时一般的网址经哪里走；还没读到时为 None
    system: Option<Route>,
    /// 最近一次没存进去的原因
    error: Option<String>,
    _system: Task<()>,
    _typed: Subscription,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    /// `folder` 为新建任务默认保存到的目录（设置里的，没设过时为系统的默认目录），`proxy` 为存着的代理。
    pub fn new(
        service: Rc<Service>,
        folder: PathBuf,
        home: Option<PathBuf>,
        proxy: Proxy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder(PROXY_EXAMPLE));
        if let Proxy::Url(url) = &proxy {
            address.update(cx, |input, cx| input.set_value(url.to_string(), window, cx));
        }
        let typed = cx.subscribe_in(
            &address,
            window,
            |this: &mut Self, _, event: &InputEvent, _, cx| match event {
                InputEvent::PressEnter { .. } => this.save_custom(cx),
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        // 读系统代理要调操作系统的接口，在后台读
        let reading = cx.background_executor().spawn(async { Route::of_system() });
        let system = cx.spawn(async move |this, cx| {
            let route = reading.await;
            // 关了时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                this.system = Some(route);
                cx.notify();
            });
        });
        let mode = match proxy {
            Proxy::System => Mode::System,
            Proxy::Direct => Mode::Direct,
            Proxy::Url(_) => Mode::Custom,
        };
        SettingsView {
            service,
            folder,
            home,
            proxy,
            mode,
            address,
            system: None,
            error: None,
            _system: system,
            _typed: typed,
        }
    }

    /// 选了一种方式：跟随系统、直连即存；指定代理要填了地址再存。
    fn pick(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.error = None;
        match mode {
            Mode::System => self.save(Proxy::System),
            Mode::Direct => self.save(Proxy::Direct),
            Mode::Custom => {}
        }
        cx.notify();
    }

    /// 存输入框里的代理地址。
    fn save_custom(&mut self, cx: &mut Context<Self>) {
        let text = self.address.read(cx).value().trim().to_owned();
        match Url::parse(&text) {
            Ok(url) => self.save(Proxy::Url(url)),
            Err(_) => self.error = Some(format!("不是代理地址，应形如 {PROXY_EXAMPLE}")),
        }
        cx.notify();
    }

    fn save(&mut self, proxy: Proxy) {
        match self.service.set_proxy(&proxy) {
            Ok(()) => self.proxy = proxy,
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn change_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("存到这里".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = picked.await;
            // 选择期间视图被关掉时不再需要结果
            let _closed = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(mut paths))) if !paths.is_empty() => {
                        let folder = paths.swap_remove(0);
                        match this.service.set_folder(&folder) {
                            Ok(()) => {
                                this.folder = folder.clone();
                                cx.emit(SettingsEvent::FolderChanged(folder));
                            }
                            Err(error) => this.error = Some(error.to_string()),
                        }
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => this.error = Some(format!("打不开目录选择：{error}")),
                    Err(_) => this.error = Some("目录选择没有返回结果".to_owned()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_proxy(&self, cx: &mut Context<Self>) -> Div {
        let chip = |id: &'static str, label: &'static str, mode: Mode, cx: &mut Context<Self>| {
            ui::chip(
                id,
                label,
                self.mode == mode,
                cx.listener(move |this, _, _, cx| this.pick(mode, cx)),
            )
        };
        let line = match self.mode {
            Mode::System => self.system.as_ref().map(|route| match route {
                Route::Proxy(proxy, source) => ui::num(
                    div()
                        .text_size(px(13.))
                        .child(format!("经代理 {proxy}（{source}）")),
                ),
                Route::Direct(why) => div().text_size(px(13.)).child(format!("直连 · {why}")),
                Route::Broken(why) => ui::error_text(why.clone()),
            }),
            Mode::Direct => Some(div().text_size(px(13.)).child("不经任何代理")),
            Mode::Custom => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(ui::BUTTON_HEIGHT))
                                    .px(px(6.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(12.))
                                    .border_1()
                                    .border_color(theme::edge())
                                    .bg(theme::ground())
                                    .text_size(px(13.))
                                    .child(Input::new(&self.address).appearance(false)),
                            )
                            .child(ui::button(
                                "save-proxy",
                                "保存",
                                Tone::Glow,
                                cx.listener(|this, _, _, cx| this.save_custom(cx)),
                            )),
                    )
                    .child(ui::caption(
                        "http、https、socks4、socks4a、socks5 或 socks5h 开头，可带用户名与密码",
                    )),
            ),
        };
        section("代理")
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(chip("proxy-system", "跟随系统", Mode::System, cx))
                    .child(chip("proxy-direct", "直连", Mode::Direct, cx))
                    .child(chip("proxy-custom", "指定代理", Mode::Custom, cx)),
            )
            .children(line)
            .child(ui::caption(
                match hs_m3u8_webview::custom_proxy_supported() {
                    Ok(()) => "在下的任务暂停再继续、嗅探页重新打开后才换".to_owned(),
                    Err(reason) => {
                        format!("在下的任务暂停再继续后才换；嗅探页的网页照系统代理：{reason}")
                    }
                },
            ))
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let folder = section("保存到").child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .child(ui::num(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(15.))
                        .child(crate::format::path(&self.folder, self.home.as_deref())),
                ))
                .child(ui::button(
                    "settings-folder",
                    "更改",
                    Tone::Plain,
                    cx.listener(|this, _, _, cx| this.change_folder(cx)),
                )),
        );
        let about = section("关于").child(ui::num(
            div()
                .text_size(px(15.))
                .child(concat!("hs-m3u8 ", env!("CARGO_PKG_VERSION"))),
        ));
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .justify_center()
            .pt(px(8.))
            .pb(px(BOTTOM))
            .child(
                div()
                    .flex_1()
                    .max_w(px(MAX_WIDTH))
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(12.))
                            .px(px(4.))
                            .pb(px(6.))
                            .child(
                                div()
                                    .text_size(px(20.))
                                    .line_height(px(28.))
                                    .font_weight(FontWeight::BOLD)
                                    .child("设置"),
                            )
                            .child(ui::button(
                                "close-settings",
                                "关闭",
                                Tone::Ghost,
                                cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Dismissed)),
                            )),
                    )
                    .child(folder)
                    .child(self.render_proxy(cx))
                    .when_some(self.error.clone(), |page, error| {
                        page.child(ui::error_text(error))
                    })
                    .child(about),
            )
    }
}

/// 一节：深色底、圆角，上面是小标题。
fn section(title: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .px(px(22.))
        .py(px(20.))
        .rounded(px(18.))
        .bg(theme::panel())
        .border_1()
        .border_color(theme::line())
        .child(ui::caption(title))
}
