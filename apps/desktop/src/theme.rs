//! 界面配色：深色底；发光的青色为进度与选中，橙色为重试与要处理的问题，红色为录制。

use gpui_kit::{Hsla, rgb};

/// 窗口底色
pub fn ground() -> Hsla {
    rgb(0x07080C).into()
}

/// 刻度环中间的圆盘
pub fn disc() -> Hsla {
    rgb(0x0B0E15).into()
}

/// 卡片、提示条、粘贴栏的底色
pub fn panel() -> Hsla {
    rgb(0x0E1118).into()
}

/// 按钮的底色、列表项悬停的底色
pub fn raised() -> Hsla {
    rgb(0x0F131C).into()
}

/// 选中的列表项、按钮悬停的底色
pub fn selected() -> Hsla {
    rgb(0x111827).into()
}

/// 描边里最暗的一档：空刻度、进度环的底、普通信号卡片
pub fn line() -> Hsla {
    rgb(0x1A2030).into()
}

/// 没选中的清晰度环
pub fn ring_idle() -> Hsla {
    rgb(0x1E2636).into()
}

/// 悬停时的描边（清晰度环、胶囊、按钮）、没选中的清晰度标签前的圆点
pub fn hover_edge() -> Hsla {
    rgb(0x2C3850).into()
}

/// 描边里中间的一档：选中的列表项、粘贴栏、剪贴板卡片、清晰度环的引线
pub fn edge() -> Hsla {
    rgb(0x22304A).into()
}

/// 描边里较亮的一档：按钮与没选中的胶囊
pub fn stroke() -> Hsla {
    rgb(0x2A3346).into()
}

pub fn text() -> Hsla {
    rgb(0xEDEFF5).into()
}

/// 列表小环里的字、没选中的胶囊上的字
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

/// 写出中的列表小环、青色按钮与链接悬停时
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
pub fn written() -> Hsla {
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

/// 不再需要注意的：已完成任务的列表小环、失败与放弃中任务的刻度
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
