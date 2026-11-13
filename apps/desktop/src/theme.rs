//! 「轨道」方向的配色：深色底，发光的青色为进度，橙色为重试与要处理的问题，红色为录制。

use gpui_kit::{Hsla, rgb};

/// 窗口底色
pub fn ground() -> Hsla {
    rgb(0x07080C).into()
}

/// 刻度环中间的圆盘
pub fn disc() -> Hsla {
    rgb(0x0B0E15).into()
}

/// 卡片、粘贴栏的底色
pub fn panel() -> Hsla {
    rgb(0x0E1118).into()
}

/// 按钮底色与列表项悬停
pub fn raised() -> Hsla {
    rgb(0x0F131C).into()
}

/// 选中的列表项
pub fn selected() -> Hsla {
    rgb(0x111827).into()
}

/// 空刻度、进度环的底、普通卡片的描边
pub fn line() -> Hsla {
    rgb(0x1A2030).into()
}

/// 没选中的清晰度环
pub fn ring_idle() -> Hsla {
    rgb(0x1E2636).into()
}

/// 悬停的清晰度环、没选中的标签圆点
pub fn ring_hover() -> Hsla {
    rgb(0x2C3850).into()
}

/// 选中项、粘贴栏的描边
pub fn edge() -> Hsla {
    rgb(0x22304A).into()
}

/// 按钮与胶囊的描边
pub fn stroke() -> Hsla {
    rgb(0x2A3346).into()
}

pub fn text() -> Hsla {
    rgb(0xEDEFF5).into()
}

/// 列表小环里的字
pub fn soft() -> Hsla {
    rgb(0xC9CEDB).into()
}

/// 次要文字；在 [`ground`] 上对比度约 6:1
pub fn muted() -> Hsla {
    rgb(0x8C93A6).into()
}

/// 进度、选中、主按钮
pub fn cyan() -> Hsla {
    rgb(0x5BE7F5).into()
}

/// 写出中的列表小环
pub fn cyan_pale() -> Hsla {
    rgb(0xA8F3FA).into()
}

/// 正在下的刻度、暂停的已完成刻度
pub fn cyan_dim() -> Hsla {
    rgb(0x2C7F88).into()
}

/// 青色实心上的文字
pub fn on_cyan() -> Hsla {
    rgb(0x04161A).into()
}

/// 已写出的刻度
pub fn white() -> Hsla {
    rgb(0xE8ECF5).into()
}

/// 重试、需要处理
pub fn orange() -> Hsla {
    rgb(0xFF9F43).into()
}

/// 录制中
pub fn red() -> Hsla {
    rgb(0xFF5A5F).into()
}

/// 已完成的任务（不再需要注意）、失败任务已完成的刻度
pub fn finished() -> Hsla {
    rgb(0x3B4253).into()
}

/// 重试类信号卡片的描边
pub fn warn_edge() -> Hsla {
    rgb(0x5A3A18).into()
}

/// 失败类信号卡片的描边
pub fn error_edge() -> Hsla {
    rgb(0x5A1F22).into()
}

/// `color` 换成透明度 `alpha`（0 到 1），发光与半透明用。
pub fn alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}
