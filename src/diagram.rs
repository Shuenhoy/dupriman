//! 放纸示意图：把「正面已打印的纸如何放回纸盒」显示成一张图。
//!
//! 图源是 `assets/paper-orientation.svg`（唯一可编辑的源文件），由
//! `scripts/render-diagram.sh` 渲染成 PNG 内嵌进二进制（改图后记得重跑）。
//!
//! 不直接内嵌 SVG 是因为 gdk-pixbuf 不一定带 SVG loader（Arch 的 librsvg
//! 只提供 `rsvg-convert`，不装 loader），而 PNG loader 是 GTK 自带的。

use gtk4::prelude::*;

/// 显示尺寸（逻辑像素）。SVG 按 2x 渲染，因此 HiDPI 下依然清晰。
pub const WIDTH: i32 = 430;
pub const HEIGHT: i32 = 210;

/// 构造示意图控件。图片加载失败时返回 None，调用方应跳过它。
pub fn paper_diagram() -> Option<gtk4::DrawingArea> {
    // 用 DrawingArea 而不是 Image/Picture：后两者的自然尺寸跟着纹理走
    // （这里是 2x 光栅），会把对话框撑大一倍，而 DrawingArea 只认
    // content_width/content_height。cairo 负责按需缩放，HiDPI 下也清晰。
    let png = include_bytes!("../assets/paper-orientation.png");
    let surface = gtk4::cairo::ImageSurface::create_from_png(&mut &png[..]).ok()?;

    let area = gtk4::DrawingArea::builder()
        .content_width(WIDTH)
        .content_height(HEIGHT)
        .build();
    area.set_halign(gtk4::Align::Center);
    area.set_draw_func(move |_, cr, width, height| {
        cr.scale(
            width as f64 / surface.width() as f64,
            height as f64 / surface.height() as f64,
        );
        cr.set_source_surface(&surface, 0.0, 0.0).ok();
        cr.paint().ok();
    });
    Some(area)
}
