//! 设置页：新建任务默认保存到哪、请求经哪个代理、BT 下完后的做种与上传限速、外观、版本与第三方声明。改了即存进任务存储（见 [`hs_dlx_app::Settings`]），存进了
//! 界面才换。

use std::num::NonZeroU32;
use std::path::PathBuf;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Div, Entity, EventEmitter, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, px,
};
use hs_dlx_app::{Seeding, Settings};
use hs_m3u8_core::{Proxy, Url};

use crate::route::Route;
use crate::tasks::Tasks;
use crate::theme::{self, Theme};
use crate::tray::Tray;
use crate::ui::{self, Tone};
use crate::web_route::WebRoute;

/// 设置各行的最大宽度。
const MAX_WIDTH: f32 = 720.;
/// 每行左边名字一栏的宽。
const LABEL_WIDTH: f32 = 160.;
/// 指定代理时输入框里的示例。
const PROXY_EXAMPLE: &str = "socks5://127.0.0.1:1080";
/// 做种的几种选择：名字与设置。「分享率 1.0 或 24 小时」为默认（见 [`Seeding::default`]）
const SEEDING_CHOICES: [(&str, &str); 3] = [
    ("seed-off", "下完不做种"),
    ("seed-default", "分享率 1.0 或 24 小时"),
    ("seed-forever", "一直做，手动停"),
];
/// 上传限速的几档：名字与字节每秒（None 为不限）。
const UPLOAD_CHOICES: [(&str, &str, Option<u32>); 4] = [
    ("upload-none", "不限", None),
    ("upload-512k", "512 KB/s", Some(512 << 10)),
    ("upload-2m", "2 MB/s", Some(2 << 20)),
    ("upload-10m", "10 MB/s", Some(10 << 20)),
];

/// 发给外面的事件。
pub enum SettingsEvent {
    Dismissed,
}

/// 代理的三种方式；选中「指定代理」后要填了地址、保存才生效。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    System,
    Direct,
    Custom,
}

pub struct SettingsView {
    /// 保存目录在这里（见 [`Tasks::set_folder`]）
    tasks: Entity<Tasks>,
    home: Option<PathBuf>,
    /// 存着的代理
    proxy: Proxy,
    /// 选中的方式：选了「指定代理」而还没保存地址时与 `proxy` 不同
    mode: Mode,
    address: Entity<InputState>,
    /// 跟随系统时一般的网址经哪里走；还没读到时为 None
    system: Option<Route>,
    /// 保存目录最近一次没换成的原因
    folder_error: Option<String>,
    /// 代理最近一次没存进去的原因
    proxy_error: Option<String>,
    /// 外观最近一次没存进去的原因
    theme_error: Option<String>,
    /// 存着的做种设置与上传限速
    seeding: Seeding,
    upload_limit: Option<NonZeroU32>,
    /// 做种或上传限速最近一次没存进去的原因
    seeding_error: Option<String>,
    /// 第三方声明打不开的原因
    notices_error: Option<String>,
    _system: Task<()>,
    _typed: Subscription,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    /// `settings` 为存着的设置。
    pub fn new(
        tasks: Entity<Tasks>,
        home: Option<PathBuf>,
        settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let proxy = settings.proxy;
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
                    this.proxy_error = None;
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
        let mode = mode_of(&proxy);
        SettingsView {
            tasks,
            home,
            proxy,
            mode,
            address,
            system: None,
            folder_error: None,
            proxy_error: None,
            theme_error: None,
            seeding: settings.seeding,
            upload_limit: settings.upload_limit,
            seeding_error: None,
            notices_error: None,
            _system: system,
            _typed: typed,
        }
    }

    /// 选了一种方式：跟随系统、直连即存，存进了才选中；指定代理要填了地址再存。
    fn pick(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.proxy_error = None;
        match mode {
            Mode::System => self.save(Proxy::System, cx),
            Mode::Direct => self.save(Proxy::Direct, cx),
            Mode::Custom => self.mode = Mode::Custom,
        }
        cx.notify();
    }

    /// 存输入框里的代理地址。
    fn save_custom(&mut self, cx: &mut Context<Self>) {
        let text = self.address.read(cx).value().trim().to_owned();
        match Url::parse(&text) {
            Ok(url) => self.save(Proxy::Url(url), cx),
            Err(_) => self.proxy_error = Some(format!("不是代理地址，应形如 {PROXY_EXAMPLE}")),
        }
        cx.notify();
    }

    /// 存 `proxy`：存进了才换成它，没存进时选中的方式回到存着的。
    fn save(&mut self, proxy: Proxy, cx: &mut Context<Self>) {
        match self.tasks.read(cx).service().set_proxy(&proxy) {
            Ok(()) => {
                if let Some(route) = WebRoute::existing(cx) {
                    route.update(cx, |route, _| route.setting_changed(&proxy));
                }
                self.mode = mode_of(&proxy);
                self.proxy = proxy;
            }
            Err(error) => {
                self.mode = mode_of(&self.proxy);
                self.proxy_error = Some(error.to_string());
            }
        }
    }

    /// 换外观：存进了才换，整个窗口随之重画。
    fn pick_theme(&mut self, picked: Theme, cx: &mut Context<Self>) {
        match self.tasks.read(cx).service().set_theme(picked) {
            Ok(()) => {
                self.theme_error = None;
                theme::set(picked);
                Tray::theme_changed(&self.tasks, cx);
                cx.refresh_windows();
            }
            Err(error) => self.theme_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn render_theme(&self, cx: &mut Context<Self>) -> Div {
        let chip = |id: &'static str, label: &'static str, value: Theme, cx: &mut Context<Self>| {
            ui::chip(
                id,
                label,
                theme::current() == value,
                cx.listener(move |this, _, _, cx| this.pick_theme(value, cx)),
            )
        };
        row(
            "外观",
            None,
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.))
                        .child(chip("theme-eclipse", "日蚀", Theme::Eclipse, cx))
                        .child(chip("theme-blood-moon", "血月", Theme::BloodMoon, cx)),
                )
                .children(self.theme_error.clone().map(ui::error_text)),
        )
    }

    /// 换做种的设置：存进了才选中，在做种的随即照它判停不停。
    fn pick_seeding(&mut self, picked: Seeding, cx: &mut Context<Self>) {
        match self.tasks.read(cx).service().set_seeding(picked) {
            Ok(()) => {
                self.seeding = picked;
                self.seeding_error = None;
            }
            Err(error) => self.seeding_error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// 换上传限速：存进了才选中。
    fn pick_upload(&mut self, picked: Option<NonZeroU32>, cx: &mut Context<Self>) {
        match self.tasks.read(cx).service().set_upload_limit(picked) {
            Ok(()) => {
                self.upload_limit = picked;
                self.seeding_error = None;
            }
            Err(error) => self.seeding_error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// 做种与上传限速两行；存着的不是这里的几种选择之一时都不选中。
    fn render_seeding(&self, cx: &mut Context<Self>) -> [Div; 2] {
        let choices = [
            Seeding::Off,
            Seeding::default(),
            Seeding::Until {
                ratio_percent: None,
                minutes: None,
            },
        ];
        let seeding = SEEDING_CHOICES
            .into_iter()
            .zip(choices)
            .map(|((id, label), value)| {
                ui::chip(
                    id,
                    label,
                    self.seeding == value,
                    cx.listener(move |this, _, _, cx| this.pick_seeding(value, cx)),
                )
            });
        let upload = UPLOAD_CHOICES.into_iter().map(|(id, label, limit)| {
            let value = limit.and_then(NonZeroU32::new);
            ui::chip(
                id,
                label,
                self.upload_limit == value,
                cx.listener(move |this, _, _, cx| this.pick_upload(value, cx)),
            )
        });
        [
            row(
                "做种",
                Some("BT 任务下完后接着上传给别人；退出应用时停下，下次打开接着做"),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(div().flex().flex_wrap().gap(px(8.)).children(seeding))
                    .children(self.seeding_error.clone().map(ui::error_text)),
            ),
            row(
                "上传限速",
                Some("BT 所有任务合计"),
                div().flex().flex_wrap().gap(px(8.)).children(upload),
            ),
        ]
    }

    /// 在访达（资源管理器、文件管理器）里打开随安装包附上的第三方声明目录；不是从安装包运行时写明原因。
    fn open_notices(&mut self, cx: &mut Context<Self>) {
        match installed_notices() {
            Ok(folder) => {
                self.notices_error = None;
                cx.open_with_system(&folder);
            }
            Err(reason) => self.notices_error = Some(reason),
        }
        cx.notify();
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
                        this.folder_error = this
                            .tasks
                            .update(cx, |tasks, cx| tasks.set_folder(folder, cx))
                            .err();
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => this.folder_error = Some(format!("打不开目录选择：{error}")),
                    Err(_) => this.folder_error = Some("目录选择没有返回结果".to_owned()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_proxy(&self, cx: &mut Context<Self>) -> Div {
        // 选了指定代理、还没存进地址，或改了地址还没保存：写明此刻照旧按什么走
        let typed = self.address.read(cx).value().trim().to_owned();
        let unsaved = match (self.mode, &self.proxy) {
            (Mode::Custom, Proxy::System) => Some("保存之前照旧跟随系统".to_owned()),
            (Mode::Custom, Proxy::Direct) => Some("保存之前照旧直连".to_owned()),
            // 按解析后的比，写法不同而地址相同（如末尾的斜杠）不算改了
            (Mode::Custom, Proxy::Url(url)) if Url::parse(&typed).ok().as_ref() != Some(url) => {
                Some(format!(
                    "改的地址还没保存，照旧经 {}",
                    hs_m3u8_core::bare_url(url)
                ))
            }
            _ => None,
        };
        let chip = |id: &'static str, label: &'static str, mode: Mode, cx: &mut Context<Self>| {
            ui::chip(
                id,
                label,
                self.mode == mode,
                cx.listener(move |this, _, _, cx| this.pick(mode, cx)),
            )
        };
        let now = |value: String| ui::reads(Vec::new(), Some(("现在".into(), value.into())));
        let line = match self.mode {
            Mode::System => self.system.as_ref().map(|route| match route {
                Route::Proxy(proxy, source) => now(format!("经 {proxy}（{source}）")),
                Route::Direct(why) => now(format!("直连 · {why}")),
                Route::Broken(why) => ui::error_text(why.clone()),
            }),
            Mode::Direct => Some(now("不经任何代理".to_owned())),
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
                                ui::input_frame()
                                    .flex_1()
                                    .px(px(6.))
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
                    ))
                    .children(unsaved.map(ui::caption)),
            ),
        };
        row(
            "代理",
            Some("探测、下载与嗅探页的网页经哪里出去"),
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
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
            .children(self.proxy_error.clone().map(ui::error_text))
            .child(ui::caption(
                match hs_dlx_webview::custom_proxy_supported() {
                    Ok(()) if hs_dlx_webview::PROXY_FIXED_FOR_PROCESS => "在下的任务暂停再继续后才换；嗅探页的网页在直连与指定代理之间立即换，与跟随系统之间换要重启应用".to_owned(),
                    Ok(()) => "在下的任务暂停再继续后才换；嗅探页的网页在直连与指定代理之间立即换，与跟随系统之间换要把它们都关掉再开".to_owned(),
                    Err(reason) => format!(
                        "在下的任务暂停再继续后才换；{reason}，直连或指定代理时嗅探页打不开网页"
                    ),
                },
            )),
        )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let folder = row(
            "保存到",
            Some("新建任务默认存到这里，每个任务可以另选"),
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            ui::field(crate::format::path(
                                self.tasks.read(cx).folder(),
                                self.home.as_deref(),
                            ))
                            .flex_1(),
                        )
                        .child(ui::button(
                            "settings-folder",
                            "更改",
                            Tone::Plain,
                            cx.listener(|this, _, _, cx| this.change_folder(cx)),
                        )),
                )
                .children(self.folder_error.clone().map(ui::error_text)),
        );
        let about = row(
            "关于",
            None,
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .child(ui::num(
                            div()
                                .text_size(px(15.))
                                .child(concat!("hs-dlx ", env!("CARGO_PKG_VERSION"))),
                        ))
                        .child(ui::button(
                            "settings-notices",
                            "第三方声明",
                            Tone::Plain,
                            cx.listener(|this, _, _, cx| this.open_notices(cx)),
                        )),
                )
                .children(self.notices_error.clone().map(ui::error_text)),
        );
        ui::deck()
            .flex_1()
            .min_w_0()
            .child(ui::page_head(
                None,
                "设置",
                None,
                [ui::button(
                    "close-settings",
                    "关闭",
                    Tone::Ghost,
                    cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Dismissed)),
                )
                .into_any_element()],
            ))
            .child(
                div()
                    .id("settings")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .justify_center()
                    .px(px(24.))
                    .pt(px(8.))
                    .pb(px(24.))
                    .child(
                        div()
                            .flex_1()
                            .max_w(px(MAX_WIDTH))
                            .flex()
                            .flex_col()
                            .child(folder.border_b_1().border_color(theme::hairline()))
                            .child(
                                self.render_proxy(cx)
                                    .border_b_1()
                                    .border_color(theme::hairline()),
                            )
                            .children(
                                self.render_seeding(cx)
                                    .map(|row| row.border_b_1().border_color(theme::hairline())),
                            )
                            .child(
                                self.render_theme(cx)
                                    .border_b_1()
                                    .border_color(theme::hairline()),
                            )
                            .child(about),
                    ),
            )
    }
}

/// 随安装包附上的第三方声明目录（打包时由 cargo-packager 当作资源放进，见 apps/desktop/Cargo.toml）：macOS 在
/// `.app/Contents/Resources/LICENSES`，Windows 在程序旁的 `LICENSES`，Linux 在 `/usr/lib/<程序名>/LICENSES`（deb）。找不到（从源码直接运行）时为原因。
fn installed_notices() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| format!("找不到程序所在的位置：{error}"))?;
    let beside = exe.parent().ok_or("程序没有所在的目录")?;
    let folder = if cfg!(target_os = "macos") {
        beside.join("../Resources/LICENSES")
    } else if cfg!(target_os = "windows") {
        beside.join("LICENSES")
    } else {
        let name = exe.file_name().ok_or("程序没有文件名")?;
        beside.join("../lib").join(name).join("LICENSES")
    };
    if folder.join("NOTICE.txt").is_file() {
        Ok(folder)
    } else {
        Err("这不是安装包里的程序，第三方声明在源码的 apps/desktop/LICENSES 下".to_owned())
    }
}

/// 存着 `proxy` 时选中的方式。
fn mode_of(proxy: &Proxy) -> Mode {
    match proxy {
        Proxy::System => Mode::System,
        Proxy::Direct => Mode::Direct,
        Proxy::Url(_) => Mode::Custom,
    }
}

/// 一行：左边是名字与它管什么（`about`），右边是 `content`。
fn row(title: &'static str, about: Option<&'static str>, content: Div) -> Div {
    div()
        .flex()
        .gap(px(24.))
        .py(px(22.))
        .child(
            div()
                .w(px(LABEL_WIDTH))
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .children(about.map(|about| {
                    div()
                        .text_size(px(12.))
                        .line_height(px(ui::CAPTION_LINE))
                        .text_color(theme::faint())
                        .child(about)
                })),
        )
        .child(content.flex_1().min_w_0())
}
