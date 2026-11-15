//! 渲染一帧背景存成 PNG，并量连续渲染的耗时：`cargo run -p hs-dlx-scene --example frame -- <输出.png> [每点像素数]`。
//! 用来在不开应用的情况下看着色器的样子；要有 GPU。

use std::time::Instant;

use hs_dlx_scene::{Palette, Params, Renderer};

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("用法：frame <输出.png> [每点像素数]");
    let scale: f32 = args
        .next()
        .map_or(1.0, |s| s.parse().expect("每点像素数是数字"));
    let (w, h) = ((1400.0 * scale) as u32, (860.0 * scale) as u32);
    let params = Params {
        width: w,
        height: h,
        scale,
        phase: 0.3,
        center: [700.0 * scale, 380.0 * scale],
        radius: 150.0 * scale,
        strength: 1.0,
        flow: 12.0,
        palette: Palette {
            light: [0.25, 0.85, 1.0],
            deep: [0.10, 0.18, 1.0],
            core: [0.85, 0.97, 1.0],
        },
        ground: [0.020, 0.027, 0.043],
        deck: [0.039, 0.055, 0.086],
        panels: [
            Some([18.0 * scale, 46.0 * scale, 288.0 * scale, 842.0 * scale]),
            Some([1062.0 * scale, 46.0 * scale, 1382.0 * scale, 842.0 * scale]),
            None,
            None,
        ],
        panel_radius: 19.0,
    };
    let mut renderer = Renderer::new().expect("建渲染器");
    let frame = renderer.render(&params).expect("渲染");
    let started = Instant::now();
    for i in 0..30 {
        let mut next = params;
        next.phase = i as f32 / 30.0;
        renderer.render(&next).expect("渲染");
    }
    println!("{w}×{h}：每帧 {:?}", started.elapsed() / 30);
    let mut rgba = frame.bgra;
    for pixel in rgba.chunks_mut(4) {
        pixel.swap(0, 2);
    }
    image::save_buffer(
        &out,
        &rgba,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgba8,
    )
    .expect("写 PNG");
}
