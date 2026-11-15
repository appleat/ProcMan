#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! procman —— Windows 原生进程管理器
//!
//! 三个核心能力：
//! 1. 查看系统当前进程（PID / CPU / 内存 / 状态 / 用户 / 路径）
//! 2. 结束进程（TerminateProcess，带权限错误提示）
//! 3. 按端口反查占用进程（TCP/UDP，含 IPv4/IPv6）

mod app;
mod data;
#[cfg(feature = "screenshot")]
mod shot;
mod winutil;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1140.0, 720.0])
            .with_min_inner_size([820.0, 480.0])
            .with_title("进程管理器 · procman"),
        ..Default::default()
    };

    eframe::run_native(
        "procman",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
