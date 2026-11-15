//! 界面配色。底色由暗到亮分三层：窗口底色（中间的舞台直接在它上面；输入框也用它，在面板上显得凹下去）、面板（任务栏、
//! 检查面板、整页面板）、面板里的卡片与读数格。颜色即含义：发光的主色为在进行与可操作，警告色为重试与要处理的问题，
//! 直播色为录制，失败级别的信号另有一色。
//!
//! 主色、警告色与直播色随主题（[`Theme`]）：日蚀为青色与深蓝，血月为火红与暗红；中性的底色、线与字两套共用。主题是
//! 整个应用的状态（只有一个窗口），启动时按设置由 [`set`] 定，在设置页换了之后要让窗口重画。

use std::sync::atomic::{AtomicU8, Ordering};

use gpui_kit::{Hsla, rgb};
pub use hs_dlx_app::Theme;

/// 当前主题：0 为日蚀，1 为血月。
static CURRENT: AtomicU8 = AtomicU8::new(0);

/// 换主题；之后取的颜色都按它。
pub fn set(theme: Theme) {
    let code = match theme {
        Theme::Eclipse => 0,
        Theme::BloodMoon => 1,
    };
    CURRENT.store(code, Ordering::Relaxed);
}

/// 当前主题。
pub fn current() -> Theme {
    match CURRENT.load(Ordering::Relaxed) {
        0 => Theme::Eclipse,
        _ => Theme::BloodMoon,
    }
}

/// 按当前主题取 `eclipse` 或 `blood_moon` 里的一色。
fn pick(eclipse: u32, blood_moon: u32) -> Hsla {
    rgb(match current() {
        Theme::Eclipse => eclipse,
        Theme::BloodMoon => blood_moon,
    })
    .into()
}

/// 窗口底色：中间的舞台直接在它上面
pub fn ground() -> Hsla {
    rgb(0x05070B).into()
}

/// 面板：任务栏、检查面板与整页面板的底色
pub fn deck() -> Hsla {
    rgb(0x0A0E16).into()
}

/// 面板的描边
pub fn deck_edge() -> Hsla {
    rgb(0x18202E).into()
}

/// 面板里分隔标题栏、底栏与各段的细线
pub fn hairline() -> Hsla {
    rgb(0x141B28).into()
}

/// 读数格与面板里的卡片的底色
pub fn cell() -> Hsla {
    rgb(0x0C111B).into()
}

/// 读数格的标签、时间：最次一级、只作标注的字
pub fn faint() -> Hsla {
    rgb(0x737C94).into()
}

/// 刻度环中间的圆盘
pub fn disc() -> Hsla {
    rgb(0x080B12).into()
}

/// 可点的东西静止时的底色，以及悬停时稍微抬起的底色
pub fn raised() -> Hsla {
    rgb(0x0F131C).into()
}

/// 选中与按下前（悬停）的底色
pub fn selected() -> Hsla {
    rgb(0x111827).into()
}

/// 最暗的线：还没做的刻度、进度的底、普通级别的描边
pub fn line() -> Hsla {
    rgb(0x1A2030).into()
}

/// 可选、但没选中的环
pub fn ring_idle() -> Hsla {
    rgb(0x1E2636).into()
}

/// 悬停时的描边，与没选中项的标记
pub fn hover_edge() -> Hsla {
    rgb(0x2C3850).into()
}

/// 中间一档的线：面的轮廓、选中项的描边、引线
pub fn edge() -> Hsla {
    rgb(0x22304A).into()
}

/// 较亮的线：可点的东西的轮廓
pub fn stroke() -> Hsla {
    rgb(0x2A3346).into()
}

/// 正文
pub fn text() -> Hsla {
    rgb(0xEDEFF5).into()
}

/// 比正文稍暗的字：次一级但仍要读的
pub fn soft() -> Hsla {
    rgb(0xC9CEDB).into()
}

/// 次要文字；在 [`ground`] 上对比度约 6:1
pub fn muted() -> Hsla {
    rgb(0x8C93A6).into()
}

/// 主色：进度、选中、每屏最该按的那一个
pub fn accent() -> Hsla {
    pick(0x5BE7F5, 0xFF6A3D)
}

/// 主色的浅一档：主色悬停时、写出阶段
pub fn accent_pale() -> Hsla {
    pick(0xA8F3FA, 0xFFC59E)
}

/// 主色的暗一档：不在动的进度（暂停）、正在下的刻度
pub fn accent_dim() -> Hsla {
    pick(0x2C7F88, 0x8A2E18)
}

/// 要处理的说明的底色
pub fn warn_ground() -> Hsla {
    rgb(0x140F08).into()
}

/// 主色实心上的文字
pub fn on_accent() -> Hsla {
    pick(0x04161A, 0x1F0A04)
}

/// 直播钟面上相邻会话交替的第二种颜色（第一种是主色）
pub fn session_alt() -> Hsla {
    rgb(0xE8ECF5).into()
}

/// 写出阶段已写出的部分
pub fn written() -> Hsla {
    rgb(0xE8ECF5).into()
}

/// 要留意的：重试、失败、要用户处理的提示；血月下为琥珀黄，与火红的主色分开
pub fn warn() -> Hsla {
    pick(0xFFA24C, 0xFFD25E)
}

/// 直播录制；血月下偏玫红，与火红的主色分开
pub fn live() -> Hsla {
    pick(0xFF5A5F, 0xFF4D8A)
}

/// 不再需要注意的：已完成、作废的进度
pub fn finished() -> Hsla {
    rgb(0x3B4253).into()
}

/// 要留意级别的信号的字
pub fn signal_warn() -> Hsla {
    pick(0xFFC48C, 0xFFE29A)
}

/// 失败级别的信号的字
pub fn signal_error() -> Hsla {
    rgb(0xFF8A8D).into()
}

/// 要留意级别的描边：重试类信号与要用户关掉的提示
pub fn warn_edge() -> Hsla {
    rgb(0x5A3A18).into()
}

/// `color` 换成透明度 `alpha`（0 到 1），发光与半透明用。
pub fn alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}
