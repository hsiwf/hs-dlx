//! 桌面应用的入口：打开任务存储、启动任务服务、开主窗口；任务存储已被开着的应用占着时，把启动参数里的链接转交给它
//! 后退出（见 hs-m3u8-instance）。

mod format;
mod headers;
mod links;
mod live;
mod new_task;
mod rate;
mod ring;
mod shell;
mod signals;
mod sniff;
mod theme;
mod ui;
mod wait;

use std::borrow::Cow;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use gpui_kit::component::TitleBar;
use gpui_kit::{
    AppContext as _, Bounds, FontWeight, ParentElement as _, Render, SharedString, Styled as _,
    TitlebarOptions, WindowBounds, WindowOptions, div, point, px, size,
};
use hs_m3u8_app::{Service, Store};

use crate::shell::{MIN_HEIGHT, MIN_WIDTH, Shell};
use crate::ui::{PAD, TITLE_HEIGHT};

/// 全部任务合计同时最多多少个请求（见 `Engine::new`）。
const MAX_REQUESTS: NonZeroUsize = NonZeroUsize::new(32).unwrap();

/// 随应用打包的字体：数字用的 Space Grotesk 三个字重（SIL OFL 1.1，许可证见同目录 OFL.txt）。
const FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/SpaceGrotesk-Regular.ttf"),
    include_bytes!("../assets/fonts/SpaceGrotesk-Medium.ttf"),
    include_bytes!("../assets/fonts/SpaceGrotesk-Bold.ttf"),
];

/// 任务存储的位置：系统给应用放数据的目录（macOS 为 ~/Library/Application Support）下的 hs-m3u8/tasks.sqlite。
fn store_path() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("hs-m3u8").join("tasks.sqlite"))
}

/// 新建任务默认保存到哪：影片目录，没有时下载目录，再没有时主目录。
fn default_folder() -> Option<PathBuf> {
    dirs::video_dir()
        .or_else(dirs::download_dir)
        .or_else(dirs::home_dir)
}

/// 主窗口：标题栏透明，顶栏由应用自己画（红绿灯嵌在顶栏里）。
fn window_options(cx: &gpui_kit::App) -> WindowOptions {
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

/// 打开平台。Linux 上有 X11（含 Wayland 会话里的 XWayland）时只用它：嗅探页的网页只能嵌进 X11 窗口；没有时照默认选，
/// 嗅探页打开时说明原因。
fn application() -> gpui_kit::Application {
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_some() {
        return gpui_kit::platform::linux(gpui_kit::WindowingModes::X11);
    }
    gpui_kit::application()
}

fn main() {
    // 任何线程上的 panic 都是不变量被破坏：照常打印后中止进程，不让界面带着坏掉的状态继续，也不让任务服务里的任务
    // 悄悄消失；存储里还标着运行中的任务下次打开时按暂停处理
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report(info);
        std::process::abort();
    }));
    application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            cx.text_system()
                .add_fonts(FONTS.iter().map(|font| Cow::Borrowed(*font)).collect())
                .expect("随应用打包的字体是有效的 TrueType 文件");
            // 输入框等组件跟着窗口的深色底
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
            // 主窗口关掉即退出；有任务在运行时关窗前先问（见 Shell）
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let options = window_options(cx);
            // 第一个参数为要探测的链接：从终端或别的程序把链接交给应用。不是 UTF-8 的照原样换成替换字符，粘贴栏里
            // 会提示它不是链接
            let link = std::env::args_os()
                .nth(1)
                .map(|arg| arg.to_string_lossy().into_owned());
            let started = store_path()
                .zip(default_folder())
                .ok_or_else(|| Failure::new("启动失败", "找不到放应用数据或保存视频的目录"))
                .and_then(|(path, folder)| {
                    let store = match Store::open(&path) {
                        Ok(store) => store,
                        // 已经开着一个：把链接交给它（或请它到前台）就退出
                        Err(hs_m3u8_app::Error::InUse(_)) => {
                            return match hs_m3u8_instance::hand_over(&path, link.as_deref()) {
                                Ok(()) => Ok(None),
                                Err(reason) => Err(Failure::new(
                                    "hs-m3u8 已经在运行",
                                    match &link {
                                        Some(link) => format!(
                                            "同时只能开一个，链接没能交给它（{reason}）：到已经打开的那个窗口里粘贴 {link}"
                                        ),
                                        None => format!(
                                            "同时只能开一个，没能请它到前台（{reason}）：切到已经打开的那个窗口即可"
                                        ),
                                    },
                                )),
                            };
                        }
                        Err(error) => return Err(Failure::new("启动失败", error.to_string())),
                    };
                    let handoffs = hs_m3u8_instance::listen(&path);
                    let (service, events) = Service::new(store, MAX_REQUESTS)
                        .map_err(|error| Failure::new("启动失败", error.to_string()))?;
                    Ok(Some((service, events, folder, handoffs)))
                });
            let opened = match started {
                Ok(None) => {
                    cx.quit();
                    return;
                }
                Ok(Some((service, events, folder, handoffs))) => {
                    gpui_kit::open_window(options, cx, |window, cx| {
                        cx.new(|cx| Shell::new(service, events, folder, link, handoffs, window, cx))
                    })
                    .map(drop)
                }
                Err(failure) => {
                    let options = WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                            None,
                            size(px(560.), px(320.)),
                            cx,
                        ))),
                        window_min_size: None,
                        ..window_options(cx)
                    };
                    gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| failure)).map(drop)
                }
            };
            opened.expect("开不了窗口");
        });
}

/// 启动失败时的窗口：一句标题与原因。
struct Failure {
    title: SharedString,
    detail: SharedString,
}

impl Failure {
    fn new(title: impl Into<SharedString>, detail: impl Into<SharedString>) -> Failure {
        Failure {
            title: title.into(),
            detail: detail.into(),
        }
    }
}

impl Render for Failure {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::ground())
            .text_color(theme::text())
            .child(
                TitleBar::new()
                    .h(px(TITLE_HEIGHT))
                    .bg(theme::ground())
                    .border_color(theme::ground()),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::orange())
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .max_w(px(480.))
                            .px(px(PAD))
                            .text_center()
                            .text_color(theme::muted())
                            .child(self.detail.clone()),
                    ),
            )
    }
}
