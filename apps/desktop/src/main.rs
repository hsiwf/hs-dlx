//! 桌面应用的入口：打开任务存储、启动任务服务、开主窗口。

mod format;
mod new_task;
mod ring;
mod shell;
mod signals;
mod theme;
mod ui;

use std::num::NonZeroUsize;
use std::path::PathBuf;

use gpui_kit::component::TitleBar;
use gpui_kit::{
    AppContext as _, Bounds, FontWeight, ParentElement as _, Render, Styled as _, TitlebarOptions,
    WindowBounds, WindowOptions, div, point, px, size,
};
use hs_m3u8_app::{Service, Store};

use crate::shell::Shell;

/// 全部任务合计同时最多多少个请求（见 `Engine::new`）。
const MAX_REQUESTS: NonZeroUsize = NonZeroUsize::new(32).unwrap();

/// 顶栏高度；macOS 的红绿灯按它垂直居中。
pub const TITLE_HEIGHT: f32 = 56.0;

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

/// 主窗口：标题栏透明，顶栏由应用自己画（红绿灯嵌在顶栏里），最小尺寸放得下三栏。
fn window_options(cx: &gpui_kit::App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1440.), px(900.)),
            cx,
        ))),
        window_min_size: Some(size(px(1120.), px(720.))),
        titlebar: Some(TitlebarOptions {
            title: Some("hs-m3u8".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(20.), px(TITLE_HEIGHT / 2. - 6.))),
        }),
        ..TitleBar::window_options()
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            // 输入框等组件跟着窗口的深色底
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
            // 关掉主窗口即退出；运行中的任务下次启动时记为暂停，可继续
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let options = window_options(cx);
            let started = store_path()
                .zip(default_folder())
                .ok_or_else(|| "找不到放应用数据或保存视频的目录".to_owned())
                .and_then(|(path, folder)| {
                    let store = Store::open(&path).map_err(|e| e.to_string())?;
                    let (service, events) =
                        Service::new(store, MAX_REQUESTS).map_err(|e| e.to_string())?;
                    Ok((service, events, folder))
                });
            let opened = match started {
                Ok((service, events, folder)) => {
                    // 第一个参数为要探测的链接：从终端或别的程序把链接交给应用
                    let link = std::env::args().nth(1);
                    gpui_kit::open_window(options, cx, |window, cx| {
                        cx.new(|cx| Shell::new(service, events, folder, link, window, cx))
                    })
                    .map(drop)
                }
                Err(message) => {
                    gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| Failure(message)))
                        .map(drop)
                }
            };
            opened.expect("开不了窗口");
        });
}

/// 启动失败时的窗口：说明原因。
struct Failure(String);

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
                            .child("启动失败"),
                    )
                    .child(
                        div()
                            .max_w(px(560.))
                            .text_center()
                            .text_color(theme::muted())
                            .child(self.0.clone()),
                    ),
            )
    }
}
