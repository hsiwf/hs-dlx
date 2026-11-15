//! 窗口的动态背景：日冕、绕圆盘弯曲并向外流的网格，延伸进两侧面板（着色器与渲染见 hs-dlx-scene）。
//!
//! 一个后台线程持有 GPU 渲染器，按界面送来的最新参数画一帧、读回成图；界面每隔 [`FRAME`] 送一次参数、取走画好的
//! 图，铺在窗口最底层（[`layer`]）。圆盘的中心与面板的位置不另写一份坐标：表盘与面板在布局时报上来
//! （[`report_disc`]、[`panel_floor`]），下一次送参数时用。面板自己不涂底色：铺着的图是按它现在的位置画的时，面板
//! 的底已在图里（光从朝圆盘的那条边漫进去）；不是时（刚换页、刚改大小、没有 GPU）面板自己涂纯色底。

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gpui_kit::{
    App, Bounds, Corners, Global, IntoElement, Pixels, Point, RenderImage, Size, Styled as _,
    canvas, fill, px,
};
use hs_dlx_scene::{MAX_PANELS, Palette, Params, Renderer};

use crate::{dial, theme};

/// 多久送一次参数、取一次图：每秒 30 帧。
pub const FRAME: Duration = Duration::from_millis(33);
/// 背景每个逻辑点画几个像素，高分屏上由界面放大铺满：背景都是柔和的光，开销与像素数成正比（每帧整张图要上传给
/// 界面），按屏幕的像素画在 2 倍屏上要多花 4 倍。
const DETAIL: f32 = 1.;
/// 日冕的光芒走完一个循环的时长：循环首尾相接，画面不跳。
const LOOP: Duration = Duration::from_secs(24);
/// 页面上有没有圆盘变了之后，日冕多久亮起或熄灭。
const FADE: Duration = Duration::from_millis(400);
/// 两次送参数之间最多算多长：窗口卡住之后回来，光芒与网格不一下跳一大截。
const MAX_STEP: Duration = Duration::from_millis(100);
/// 有下载时网格每秒向外流过多少逻辑点：`FLOW_BASE` 加上 `FLOW_PER_DECADE` 乘速度（MB/s 加 1）的常用对数。1 MB/s
/// 约 8 点、10 MB/s 约 12 点、100 MB/s 约 18 点；没有下载时不流。
const FLOW_BASE: f32 = 6.;
const FLOW_PER_DECADE: f32 = 6.;

/// 背景停了的原因：建不起 GPU 渲染器，或画某一帧时出错。停了之后界面一直用纯色底。
#[derive(Debug)]
pub struct Stopped(pub hs_dlx_scene::Error);

/// 窗口的动态背景：后台渲染线程、动画走到哪。由主窗口的视图持有，随它释放（释放时等渲染线程画完手上的一帧退出）。
pub struct Backdrop {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    /// 上次送参数的时刻
    last_tick: Instant,
    /// 光芒在一个循环里走到哪（0 到 1）
    phase: f32,
    /// 网格向外流过的距离（逻辑点，只增不减）
    flow: f32,
    /// 日冕的强度（0 到 1）：页面上有圆盘时向 1 走，没有时向 0 走
    strength: f32,
    /// 最近一次报上来的圆盘中心；页面上没有圆盘时日冕留在原处熄灭。一次也没报过时为 None
    disc: Option<Point<Pixels>>,
    /// 最近送出去的参数：没变就不再送，静止时不重画
    sent: Option<Params>,
    /// 已停：不再送参数、不再取图
    stopped: bool,
}

/// 界面与渲染线程之间交接的东西。
struct Shared {
    state: Mutex<State>,
    /// 有新参数或要退出时通知渲染线程
    wake: Condvar,
}

#[derive(Default)]
struct State {
    /// 等着画的一帧；渲染线程取走后为 None，界面送新的时覆盖没画的
    wanted: Option<Job>,
    /// 画好、界面还没取走的一帧（只留最新的）；渲染出错时为错误，此后渲染线程已退出
    finished: Option<Result<Painted, hs_dlx_scene::Error>>,
    /// 视图释放了，渲染线程退出
    closed: bool,
}

/// 一帧的参数与画它时量到的布局（逻辑点）。
struct Job {
    params: Params,
    measured: Measured,
    size: Size<Pixels>,
}

/// 画好的一帧。
struct Painted {
    image: Arc<RenderImage>,
    measured: Measured,
    size: Size<Pixels>,
}

/// 布局时量到的（逻辑点，窗口坐标）。
#[derive(Debug, Clone, Default, PartialEq)]
struct Measured {
    disc: Option<Point<Pixels>>,
    panels: Vec<Bounds<Pixels>>,
}

/// 窗口这一帧的大小、每点几个像素、是不是在前台。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Viewport {
    size: Size<Pixels>,
    scale: f32,
    active: bool,
}

/// 布局时报上来的位置与铺在底下的图：表盘与面板在布局时没有主窗口视图可找，经它交接。主窗口打开时重置。
#[derive(Default)]
struct Board {
    /// 这一帧布局时正在量的
    measuring: Measured,
    /// 上一帧量完的
    measured: Measured,
    /// 上一帧窗口的大小；还没画过一帧时为 None
    viewport: Option<Viewport>,
    /// 铺在底下的图与画它时的布局；还没有图或背景已停时为 None
    shown: Option<Painted>,
    /// 上一帧画出去的图：换了图之后从图集里移除它
    painted: Option<Arc<RenderImage>>,
}

impl Global for Board {}

impl Backdrop {
    /// 起渲染线程（在线程里建 GPU 渲染器，建不起时第一次 [`Backdrop::tick`] 报出原因），重置布局的交接。
    pub fn new(cx: &mut App) -> Backdrop {
        cx.set_global(Board::default());
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let worker = std::thread::Builder::new()
            .name("backdrop".to_owned())
            .spawn({
                let shared = shared.clone();
                move || work(&shared)
            })
            .expect("起一个线程只在系统资源耗尽时失败");
        Backdrop {
            shared,
            worker: Some(worker),
            last_tick: Instant::now(),
            phase: 0.,
            flow: 0.,
            strength: 0.,
            disc: None,
            sent: None,
            stopped: false,
        }
    }

    /// 走一步动画、按上一帧的布局送参数、取走画好的图。`speed` 为所有任务合计的下载速度（字节每秒）。返回是否换了
    /// 图（要重画窗口）；背景停了时返回原因，只返回这一次，此后什么也不做。
    pub fn tick(&mut self, speed: f64, cx: &mut App) -> Result<bool, Stopped> {
        if self.stopped {
            return Ok(false);
        }
        let now = Instant::now();
        let step = now
            .duration_since(self.last_tick)
            .min(MAX_STEP)
            .as_secs_f32();
        self.last_tick = now;
        let reduce_motion = cx.reduce_motion();
        let board = cx.global_mut::<Board>();
        let finished = self.shared.lock().finished.take();
        let changed = match finished {
            Some(Ok(painted)) => {
                board.shown = Some(painted);
                true
            }
            Some(Err(error)) => {
                self.stopped = true;
                board.shown = None;
                return Err(Stopped(error));
            }
            None => false,
        };
        let Some(viewport) = board.viewport else {
            return Ok(changed);
        };
        let measured = board.measured.clone();
        if viewport.size.width <= px(0.) || viewport.size.height <= px(0.) {
            return Ok(changed);
        }
        let animate = viewport.active && !reduce_motion;
        if animate {
            self.phase = (self.phase + step / LOOP.as_secs_f32()).rem_euclid(1.);
            self.flow += step * flow_rate(speed);
        }
        let target = if measured.disc.is_some() { 1. } else { 0. };
        self.strength = if animate {
            let reach = step / FADE.as_secs_f32();
            self.strength + (target - self.strength).clamp(-reach, reach)
        } else {
            target
        };
        if let Some(disc) = measured.disc {
            self.disc = Some(disc);
        }
        let center = self
            .disc
            .unwrap_or_else(|| Point::new(viewport.size.width / 2., viewport.size.height / 2.));
        let params = self.params(viewport, center, &measured.panels);
        if self.sent != Some(params) {
            self.sent = Some(params);
            let mut state = self.shared.lock();
            state.wanted = Some(Job {
                params,
                measured,
                size: viewport.size,
            });
            self.shared.wake.notify_one();
        }
        Ok(changed)
    }

    fn params(
        &self,
        viewport: Viewport,
        center: Point<Pixels>,
        panels: &[Bounds<Pixels>],
    ) -> Params {
        let scale = DETAIL.min(viewport.scale);
        let pixels = |value: Pixels| f32::from(value) * scale;
        let mut slots = [None; MAX_PANELS];
        assert!(
            panels.len() <= MAX_PANELS,
            "一页上的面板不超过 {MAX_PANELS} 块，这一帧量到 {} 块",
            panels.len()
        );
        for (slot, bounds) in slots.iter_mut().zip(panels) {
            *slot = Some([
                pixels(bounds.left()),
                pixels(bounds.top()),
                pixels(bounds.right()),
                pixels(bounds.bottom()),
            ]);
        }
        Params {
            width: pixels(viewport.size.width).round() as u32,
            height: pixels(viewport.size.height).round() as u32,
            scale,
            phase: self.phase,
            center: [pixels(center.x), pixels(center.y)],
            radius: dial::DISC_RADIUS * scale,
            strength: self.strength,
            flow: self.flow,
            palette: palette(),
            ground: srgb(theme::ground()),
            deck: srgb(theme::deck()),
            panels: slots,
            panel_radius: PANEL_RADIUS,
        }
    }
}

impl Drop for Backdrop {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.wake.notify_one();
        if let Some(worker) = self.worker.take()
            && let Err(panic) = worker.join()
        {
            std::panic::resume_unwind(panic);
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("拿着这把锁时只做赋值与取值，不会 panic")
    }
}

/// 渲染线程：建渲染器，等参数、画、交回，直到视图释放或出错。
fn work(shared: &Shared) {
    let mut renderer = match Renderer::new() {
        Ok(renderer) => renderer,
        Err(error) => {
            shared.lock().finished = Some(Err(error));
            return;
        }
    };
    loop {
        let job = {
            let mut state = shared.lock();
            loop {
                if state.closed {
                    return;
                }
                if let Some(job) = state.wanted.take() {
                    break job;
                }
                state = shared
                    .wake
                    .wait(state)
                    .expect("拿着这把锁时只做赋值与取值，不会 panic");
            }
        };
        let result = renderer.render(&job.params).map(|frame| {
            let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
                .expect("渲染器交回的像素正好是宽乘高乘 4 字节");
            Painted {
                image: Arc::new(RenderImage::new([image::Frame::new(buffer)])),
                measured: job.measured,
                size: job.size,
            }
        });
        let failed = result.is_err();
        shared.lock().finished = Some(result);
        if failed {
            return;
        }
    }
}

/// 网格每秒向外流过的逻辑点（见 [`FLOW_BASE`]）。
fn flow_rate(speed: f64) -> f32 {
    if speed <= 0. {
        return 0.;
    }
    let megabytes = (speed / 1e6) as f32;
    FLOW_BASE + FLOW_PER_DECADE * (1. + megabytes).log10()
}

/// 当前主题的光（线性的 0 到 1）。
fn palette() -> Palette {
    match theme::current() {
        theme::Theme::Eclipse => Palette {
            light: [0.25, 0.85, 1.0],
            deep: [0.10, 0.18, 1.0],
            core: [0.85, 0.97, 1.0],
        },
        theme::Theme::BloodMoon => Palette {
            light: [1.0, 0.26, 0.05],
            deep: [0.32, 0.01, 0.015],
            core: [1.0, 0.80, 0.50],
        },
    }
}

fn srgb(color: gpui_kit::Hsla) -> [f32; 3] {
    let rgba = color.to_rgb();
    [rgba.r, rgba.g, rgba.b]
}

/// 面板底的圆角：面板的圆角减去 1 点的描边（面板底铺在描边里面）。
const PANEL_RADIUS: f32 = crate::ui::DECK_RADIUS - 1.;

/// 窗口最底层：铺上最新的一帧（按它的逻辑大小从左上角铺，窗口刚改大小时露出的部分是窗口底色）。它在所有表盘与面
/// 板之前布局，借这一步把上一帧量到的位置交给 [`Backdrop::tick`]、开始量这一帧。
pub fn layer() -> impl IntoElement {
    canvas(
        |bounds, window, cx| {
            let board = cx.global_mut::<Board>();
            board.measured = std::mem::take(&mut board.measuring);
            board.viewport = Some(Viewport {
                size: bounds.size,
                scale: window.scale_factor(),
                active: window.is_window_active(),
            });
        },
        |bounds, (), window, cx| {
            let board = cx.global_mut::<Board>();
            let shown = board
                .shown
                .as_ref()
                .map(|shown| (shown.image.clone(), shown.size));
            let stale = board.painted.take_if(|painted| {
                shown
                    .as_ref()
                    .is_none_or(|(image, _)| !Arc::ptr_eq(painted, image))
            });
            board.painted = shown.as_ref().map(|(image, _)| image.clone());
            if let Some(stale) = stale {
                window
                    .drop_image(stale)
                    .expect("从图集里移除图片只改内存里的表，不会失败");
            }
            if let Some((image, size)) = shown {
                let bounds = Bounds::new(bounds.origin, size);
                window
                    .paint_image(bounds, bounds, Corners::default(), image, 0, false)
                    .expect("背景图只有一帧，第 0 帧总在");
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// 表盘在布局时报上圆盘的中心（窗口坐标）：日冕从这里放出。
pub fn report_disc(center: Point<Pixels>, cx: &mut App) {
    cx.global_mut::<Board>().measuring.disc = Some(center);
}

/// 面板的底：铺满面板（描边以内），布局时报上自己的位置；铺着的图是按这个位置画的时什么也不涂（底已在图里），否则
/// 涂面板的纯色底。
pub fn panel_floor() -> impl IntoElement {
    canvas(
        |bounds, _, cx| cx.global_mut::<Board>().measuring.panels.push(bounds),
        |bounds, (), window, cx| {
            let covered = cx.global::<Board>().shown.as_ref().is_some_and(|shown| {
                shown
                    .measured
                    .panels
                    .iter()
                    .any(|panel| same(*panel, bounds))
            });
            if !covered {
                window.paint_quad(fill(bounds, theme::deck()).corner_radii(px(PANEL_RADIUS)));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// 两个位置相差不到半个点：画图时量到的与现在的算同一处。
fn same(a: Bounds<Pixels>, b: Bounds<Pixels>) -> bool {
    let near = |x: Pixels, y: Pixels| (f32::from(x) - f32::from(y)).abs() < 0.5;
    near(a.left(), b.left())
        && near(a.top(), b.top())
        && near(a.right(), b.right())
        && near(a.bottom(), b.bottom())
}
