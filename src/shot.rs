//! 可选的自检截图（仅在 `--features screenshot` 时编译）
//!
//! 设置环境变量 `PROCMAN_SHOT=<png 路径>` 启动程序，窗口渲染完成后会把
//! 自身 framebuffer 存成 PNG 并退出。用于自动化验证界面，不影响正常使用。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use eframe::egui;

static START: OnceLock<Instant> = OnceLock::new();
static REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn tick(ctx: &egui::Context) {
    let path = match std::env::var("PROCMAN_SHOT") {
        Ok(p) if !p.is_empty() => p,
        _ => return,
    };

    let start = START.get_or_init(Instant::now);

    if !REQUESTED.load(Ordering::Relaxed) {
        // 等首帧数据到位后再拍，避免拍到"正在扫描"
        if start.elapsed().as_millis() > 2500 {
            REQUESTED.store(true, Ordering::Relaxed);
            eprintln!("shot: requesting screenshot at {}ms", start.elapsed().as_millis());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            ctx.request_repaint();
        }
        return;
    }

    let events = ctx.input(|i| i.events.clone());
    if !events.is_empty() {
        eprintln!("shot: {} events this frame", events.len());
    }
    for ev in events {
        if let egui::Event::Screenshot { image, .. } = ev {
            eprintln!("shot: got screenshot {}x{}", image.width(), image.height());
            let (w, h) = (image.width(), image.height());
            let mut buf = Vec::with_capacity(w * h * 4);
            for px in image.pixels.iter() {
                buf.extend_from_slice(&px.to_array());
            }
            match image::save_buffer(
                &path,
                &buf,
                w as u32,
                h as u32,
                image::ColorType::Rgba8,
            ) {
                Ok(_) => println!("screenshot saved: {path} ({w}x{h})"),
                Err(e) => eprintln!("screenshot failed: {e}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
