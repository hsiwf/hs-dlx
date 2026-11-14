//! 界面配色：三层材质——舱底（窗口底色，中间的舞台在这一层）、面板（任务栏、检查面板、整页面板）、面板里抬起的一层
//! （选中项、输入框、读数格）。颜色即含义：发光的青色为在进行与可操作，橙色为重试与要处理的问题，红色只用于录制。

use gpui_kit::{Hsla, rgb};

/// 窗口底色（舱底）：中间的舞台直接在它上面
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
pub fn cyan() -> Hsla {
    rgb(0x5BE7F5).into()
}

/// 主色的浅一档：主色悬停时、写出阶段
pub fn cyan_pale() -> Hsla {
    rgb(0xA8F3FA).into()
}

/// 主色的暗一档：不在动的进度（暂停）、正在下的刻度
pub fn cyan_dim() -> Hsla {
    rgb(0x2C7F88).into()
}

/// 橙色实心上的文字
pub fn on_amber() -> Hsla {
    rgb(0x1F1204).into()
}

/// 要处理的说明的底色
pub fn warn_ground() -> Hsla {
    rgb(0x140F08).into()
}

/// 青色实心上的文字
pub fn on_cyan() -> Hsla {
    rgb(0x04161A).into()
}

/// 直播钟面上相邻会话交替的第二种颜色（第一种是主色）
pub fn session_alt() -> Hsla {
    rgb(0xE8ECF5).into()
}

/// 写出阶段已写出的部分
pub fn written() -> Hsla {
    rgb(0xE8ECF5).into()
}

/// 要留意的：重试、失败、要用户处理的提示
pub fn orange() -> Hsla {
    rgb(0xFFA24C).into()
}

/// 直播录制
pub fn red() -> Hsla {
    rgb(0xFF5A5F).into()
}

/// 不再需要注意的：已完成、作废的进度
pub fn finished() -> Hsla {
    rgb(0x3B4253).into()
}

/// 要留意级别的信号的字
pub fn warn_text() -> Hsla {
    rgb(0xFFC48C).into()
}

/// 失败级别的信号的字
pub fn error_text() -> Hsla {
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
