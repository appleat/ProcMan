use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, RichText};
use egui_extras::{Column, TableBuilder};

use crate::data::{self, PortInfo, ProcInfo, Snapshot};
use crate::winutil;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Procs,
    Ports,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Pid,
    Name,
    Cpu,
    Mem,
}

/// UI 中产生的意图，先收集起来，等渲染闭包结束后再落地，避免借用冲突
enum Action {
    None,
    Select(u32),
    /// 端口行没有 PID 时也要能高亮，用「本地地址 + 端口」作为行标识；
    /// 第三个字段是与该端口关联的进程（可能为空）
    SelectPort(String, u16, Option<u32>),
    Sort(SortKey),
    AskKill(u32, String),
    Reveal(String),
    Refresh,
}

struct Toast {
    msg: String,
    ok: bool,
    at: Instant,
}

pub struct App {
    tab: Tab,
    snapshot: Option<Snapshot>,
    rx: Receiver<Snapshot>,
    tx_trigger: Sender<()>,
    interval: Arc<AtomicU64>,
    interval_secs: u64,
    auto: bool,
    search: String,
    listen_only: bool,
    selected: Option<u32>,
    /// 端口页单独记选中行：同一端口可能没有关联进程，用本地地址区分
    sel_port: Option<(String, u16)>,
    sort: (SortKey, bool),
    confirm: Option<(u32, String)>,
    toast: Option<Toast>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        setup_style(&cc.egui_ctx);

        let interval = Arc::new(AtomicU64::new(2000));
        let (rx, tx_trigger) = data::spawn(interval.clone());

        // 供脚本/自检用：PROCMAN_TAB=ports、PROCMAN_QUERY=8080 可直接落到指定视图
        let tab = match std::env::var("PROCMAN_TAB").unwrap_or_default().as_str() {
            "ports" => Tab::Ports,
            _ => Tab::Procs,
        };
        let search = std::env::var("PROCMAN_QUERY").unwrap_or_default();

        // 自检用：PROCMAN_SELECT=<pid> 预选中某进程，PROCMAN_DIALOG=1 直接弹出确认框
        let selected = std::env::var("PROCMAN_SELECT")
            .ok()
            .and_then(|v| v.parse::<u32>().ok());
        let confirm = if std::env::var("PROCMAN_DIALOG").is_ok() {
            selected.map(|pid| (pid, "demo.exe".to_string()))
        } else {
            None
        };

        Self {
            tab,
            snapshot: None,
            rx,
            tx_trigger,
            interval,
            interval_secs: 2,
            auto: true,
            search,
            listen_only: false,
            selected,
            sel_port: None,
            sort: (SortKey::Cpu, false),
            confirm,
            toast: None,
        }
    }

    fn drain_snapshots(&mut self) {
        let mut latest = None;
        while let Ok(s) = self.rx.try_recv() {
            latest = Some(s);
        }
        if let Some(s) = latest {
            self.snapshot = Some(s);
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Refresh => {
                let _ = self.tx_trigger.send(());
            }
            Action::Select(pid) => self.selected = Some(pid),
            Action::SelectPort(local, port, pid) => {
                self.sel_port = Some((local, port));
                if let Some(pid) = pid {
                    self.selected = Some(pid);
                }
            }
            Action::Sort(k) => {
                if self.sort.0 == k {
                    self.sort.1 = !self.sort.1;
                } else {
                    self.sort = (k, k == SortKey::Pid || k == SortKey::Name);
                }
            }
            Action::AskKill(pid, name) => {
                if pid == std::process::id() {
                    self.warn("不能结束本程序自身".to_string());
                } else {
                    self.confirm = Some((pid, name));
                }
            }
            Action::Reveal(path) => winutil::reveal_in_explorer(&path),
        }
    }

    fn warn(&mut self, msg: String) {
        self.toast = Some(Toast {
            msg,
            ok: false,
            at: Instant::now(),
        });
    }

    fn do_kill(&mut self, pid: u32, name: String) {
        match winutil::kill_process(pid) {
            Ok(()) => {
                self.toast = Some(Toast {
                    msg: format!("已结束 {name}（PID {pid}）"),
                    ok: true,
                    at: Instant::now(),
                });
                if self.selected == Some(pid) {
                    self.selected = None;
                }
                // 立刻刷新一次，让列表马上反映变化
                let _ = self.tx_trigger.send(());
            }
            Err(e) => self.warn(e),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_snapshots();

        // 同步自动刷新配置给采集线程
        let want = if self.auto { self.interval_secs * 1000 } else { 0 };
        if self.interval.load(Ordering::Relaxed) != want {
            self.interval.store(want, Ordering::Relaxed);
            let _ = self.tx_trigger.send(());
        }

        let mut action = Action::None;

        // 快捷键：F5 刷新 / Delete 结束选中进程（输入框聚焦时不抢键）
        if !ctx.wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
                action = Action::Refresh;
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
                if let Some(p) = self.selected_porc() {
                    let (pid, name) = (p.pid, p.name.clone());
                    action = Action::AskKill(pid, name);
                }
            }
        }

        // ---------- 顶部工具栏 ----------
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("进程管理器").strong().size(16.0));
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // 两个页面的搜索语义不同，切换时清掉，避免带着上一页的关键字看到空列表。
                // 这里必须用 `|` 而不是 `||`，否则短路会让第二个 tab 画不出来。
                let switched = ui
                    .selectable_value(&mut self.tab, Tab::Procs, "进程列表")
                    .clicked()
                    | ui.selectable_value(&mut self.tab, Tab::Ports, "端口查询")
                        .clicked();
                if switched {
                    self.search.clear();
                }

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(10.0);

                let hint = match self.tab {
                    Tab::Procs => "搜索进程名 / PID / 路径",
                    Tab::Ports => "输入端口号或进程名，例如 8080",
                };
                ui.add_sized(
                    [250.0, 24.0],
                    egui::TextEdit::singleline(&mut self.search).hint_text(hint),
                );
                if !self.search.is_empty() {
                    if ui
                        .add(egui::Button::new("×").frame(false))
                        .on_hover_text("清空")
                        .clicked()
                    {
                        self.search.clear();
                    }
                }
                if self.tab == Tab::Ports {
                    ui.separator();
                    ui.checkbox(&mut self.listen_only, "只看监听");
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⟳ 立即刷新").clicked() {
                        set_action(&mut action, Action::Refresh);
                    }
                    egui::ComboBox::from_id_salt("interval")
                        .width(72.0)
                        .selected_text(format!("{} 秒", self.interval_secs))
                        .show_ui(ui, |ui| {
                            for v in [1u64, 2, 3, 5, 10] {
                                ui.selectable_value(&mut self.interval_secs, v, format!("{v} 秒"));
                            }
                        });
                    ui.checkbox(&mut self.auto, "自动刷新");
                });
            });
            ui.add_space(8.0);
        });

        // ---------- 底部状态栏 ----------
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(4.0);
            let procs_total = self.snapshot.as_ref().map(|s| s.procs.len()).unwrap_or(0);
            let ports_total = self.snapshot.as_ref().map(|s| s.ports.len()).unwrap_or(0);
            let shown = self.snapshot.as_ref().map(|s| count_shown(s, self.tab, &self.search, self.listen_only)).unwrap_or(0);

            ui.horizontal(|ui| {
                match self.tab {
                    Tab::Procs => {
                        ui.label(format!("进程 {procs_total} 个"));
                        ui.separator();
                        ui.label(format!("当前显示 {shown} 个"));
                    }
                    Tab::Ports => {
                        ui.label(format!("端口条目 {ports_total} 条"));
                        ui.separator();
                        ui.label(format!("当前显示 {shown} 条"));
                    }
                }
                if let Some(s) = &self.snapshot {
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "{}刷新 · 耗时 {} ms",
                            elapsed_cn(s.at.elapsed()),
                            s.cost_ms
                        ))
                        .weak(),
                    );
                }
                if let Some(e) = self.snapshot.as_ref().and_then(|s| s.error.as_ref()) {
                    ui.separator();
                    ui.colored_label(Color32::from_rgb(0xC0, 0x39, 0x2B), e);
                }
            });

            ui.horizontal(|ui| {
                match self.selected_porc() {
                    Some(p) => {
                        ui.label(RichText::new(format!("已选 {}", p.name)).strong());
                        ui.label(RichText::new(format!("PID {}", p.pid)).weak());
                        ui.label(RichText::new(format!("内存 {}", fmt_mem(p.mem))).weak());
                        if !p.user.is_empty() {
                            ui.label(RichText::new(&p.user).weak());
                        }
                        if !p.exe.is_empty() {
                            ui.label(RichText::new(&p.exe).weak());
                        }

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);
                        let danger = egui::Button::new(RichText::new("结束进程").color(Color32::WHITE))
                            .fill(Color32::from_rgb(0xC0, 0x39, 0x2B));
                        if ui.add(danger).clicked() {
                            set_action(&mut action, Action::AskKill(p.pid, p.name.clone()));
                        }
                        if ui.button("打开文件位置").clicked() {
                            set_action(&mut action, Action::Reveal(p.exe.clone()));
                        }
                    }
                    None => {
                        ui.label(RichText::new("点击任意一行查看详情").weak());
                    }
                }
            });
            ui.add_space(4.0);
        });

        // ---------- 主区域 ----------
        egui::CentralPanel::default().show(ctx, |ui| {
            match &self.snapshot {
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("正在扫描系统进程…");
                        });
                    });
                }
                Some(snap) => {
                    let a = match self.tab {
                        Tab::Procs => draw_procs(ui, snap, &self.search, self.sort, self.selected),
                        Tab::Ports => draw_ports(
                            ui,
                            snap,
                            &self.search,
                            self.listen_only,
                            self.sel_port.as_ref(),
                        ),
                    };
                    set_action(&mut action, a);
                }
            }
        });

        // ---------- 确认框 ----------
        if let Some((pid, name)) = self.confirm.clone() {
            let mut close = false;
            let mut kill = false;
            egui::Window::new("确认结束进程")
                .collapsible(false)
                .resizable(false)
                .min_width(360.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("确定结束 {name}（PID {pid}）吗？")).strong());
                    ui.add_space(2.0);
                    ui.label(RichText::new("未保存的数据将会丢失，且无法恢复。").weak());
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                        let danger = egui::Button::new(RichText::new("结束进程").color(Color32::WHITE))
                            .fill(Color32::from_rgb(0xC0, 0x39, 0x2B));
                        if ui.add(danger).clicked() {
                            kill = true;
                        }
                    });
                });
            if kill {
                self.confirm = None;
                self.do_kill(pid, name);
            } else if close {
                self.confirm = None;
            }
        }

        // ---------- 轻提示 ----------
        if let Some(t) = &self.toast {
            if t.at.elapsed() > Duration::from_secs(4) {
                self.toast = None;
            } else {
                let color = if t.ok {
                    Color32::from_rgb(0x2E, 0x7D, 0x32)
                } else {
                    Color32::from_rgb(0xC0, 0x39, 0x2B)
                };
                egui::Area::new(egui::Id::new("toast"))
                    .anchor(egui::Align2::CENTER_TOP, [0.0, 74.0])
                    .order(egui::Order::Foreground)
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style())
                            .fill(color)
                            .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                            .show(ui, |ui| {
                                ui.colored_label(Color32::WHITE, &t.msg);
                            });
                    });
                ctx.request_repaint_after(Duration::from_millis(200));
            }
        }

        self.apply(action);

        #[cfg(feature = "screenshot")]
        crate::shot::tick(ctx);

        // 低频重绘：只为了让"x 秒前刷新"这类相对时间走动；
        // 数据本身由采集线程按间隔更新。过高的重绘频率会让本工具自己吃掉 CPU。
        ctx.request_repaint_after(Duration::from_millis(1000));
    }
}

impl App {
    fn selected_porc(&self) -> Option<&ProcInfo> {
        let pid = self.selected?;
        self.snapshot.as_ref()?.procs.iter().find(|p| p.pid == pid)
    }
}

/// 后发生的动作不覆盖已有动作，避免快捷键被面板里的空动作冲掉
fn set_action(dst: &mut Action, src: Action) {
    if !matches!(src, Action::None) {
        *dst = src;
    }
}

fn count_shown(snap: &Snapshot, tab: Tab, search: &str, listen_only: bool) -> usize {
    let q = search.trim().to_lowercase();
    match tab {
        Tab::Procs => snap.procs.iter().filter(|p| match_proc(p, &q)).count(),
        Tab::Ports => filter_ports(&snap.ports, &q, listen_only).len(),
    }
}

/// 端口筛选：输入纯数字时，若存在端口号完全相等的记录就只保留精确匹配，
/// 否则退回子串匹配（这样输入 808 仍能找到 8080）。
fn filter_ports<'a>(ports: &'a [PortInfo], q: &str, listen_only: bool) -> Vec<&'a PortInfo> {
    let rows: Vec<&PortInfo> = ports
        .iter()
        .filter(|p| !listen_only || p.state == "监听")
        .filter(|p| match_port(p, q))
        .collect();

    if !q.is_empty() && q.chars().all(|c| c.is_ascii_digit()) {
        let exact: Vec<&PortInfo> = rows
            .iter()
            .copied()
            .filter(|p| p.port.to_string() == q)
            .collect();
        if !exact.is_empty() {
            return exact;
        }
    }
    rows
}

fn match_proc(p: &ProcInfo, q: &str) -> bool {
    q.is_empty()
        || p.name.to_lowercase().contains(q)
        || p.pid.to_string() == q
        || p.exe.to_lowercase().contains(q)
}

fn match_port(p: &PortInfo, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    if q.chars().all(|c| c.is_ascii_digit()) {
        return p.port.to_string().contains(q);
    }
    p.pname.to_lowercase().contains(q)
        || p.local.to_lowercase().contains(q)
        || p.proto.to_lowercase().contains(q)
}

fn draw_procs(
    ui: &mut egui::Ui,
    snap: &Snapshot,
    search: &str,
    sort: (SortKey, bool),
    selected: Option<u32>,
) -> Action {
    let mut action = Action::None;
    let q = search.trim().to_lowercase();

    let mut rows: Vec<&ProcInfo> = snap.procs.iter().filter(|p| match_proc(p, &q)).collect();
    let (key, asc) = sort;
    rows.sort_by(|a, b| {
        let o = match key {
            SortKey::Pid => a.pid.cmp(&b.pid),
            SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortKey::Cpu => a.cpu.partial_cmp(&b.cpu).unwrap_or(std::cmp::Ordering::Equal),
            SortKey::Mem => a.mem.cmp(&b.mem),
        };
        if asc {
            o
        } else {
            o.reverse()
        }
    });

    // 列与列之间原本有 8px 的 item_spacing，这段缝隙不属于任何单元格，
    // 点在里面既不会选中行也不会弹菜单（表现为"点空白能选中、点缝隙没反应"）。
    // 归零后整行连续可点。
    ui.spacing_mut().item_spacing.x = 0.0;

    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        // 默认是 Sense::hover()，那样 row.response() 收不到任何点击，
        // 既不会触发选中高亮，右键菜单也永远弹不出来。必须显式开启 click。
        .sense(egui::Sense::click())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::exact(72.0))
        .column(Column::initial(240.0).at_least(120.0).clip(true))
        .column(Column::exact(76.0))
        .column(Column::exact(92.0))
        .column(Column::exact(58.0))
        .column(Column::initial(130.0).at_least(60.0).clip(true))
        .column(Column::remainder().clip(true))
        .header(24.0, |mut h| {
            h.col(|ui| {
                if sort_header(ui, "PID", key == SortKey::Pid, asc).clicked() {
                    action = Action::Sort(SortKey::Pid);
                }
            });
            h.col(|ui| {
                if sort_header(ui, "名称", key == SortKey::Name, asc).clicked() {
                    action = Action::Sort(SortKey::Name);
                }
            });
            h.col(|ui| {
                if sort_header(ui, "CPU", key == SortKey::Cpu, asc).clicked() {
                    action = Action::Sort(SortKey::Cpu);
                }
            });
            h.col(|ui| {
                if sort_header(ui, "内存", key == SortKey::Mem, asc).clicked() {
                    action = Action::Sort(SortKey::Mem);
                }
            });
            h.col(|ui| {
                ui.label(RichText::new("状态").strong());
            });
            h.col(|ui| {
                ui.label(RichText::new("用户").strong());
            });
            h.col(|ui| {
                ui.label(RichText::new("可执行文件路径").strong());
            });
        })
        .body(|body| {
            body.rows(22.0, rows.len(), |mut row| {
                let p = rows[row.index()];
                row.set_selected(selected == Some(p.pid));

                row.col(|ui| {
                    ui.monospace(p.pid.to_string());
                });
                row.col(|ui| {
                    // 原来这里挂 on_hover_text 显示父进程/命令行，但 tooltip 是独立图层，
                    // 悬停时会挡在行上，也会让人以为点击没生效。信息已移到右键「详细」里。
                    ui.label(&p.name);
                });
                row.col(|ui| {
                    let c = if p.cpu >= 50.0 {
                        Color32::from_rgb(0xC0, 0x39, 0x2B)
                    } else if p.cpu >= 10.0 {
                        Color32::from_rgb(0xE6, 0x7E, 0x22)
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.label(RichText::new(format!("{:.1}%", p.cpu)).color(c).monospace());
                });
                row.col(|ui| {
                    ui.label(RichText::new(fmt_mem(p.mem)).monospace());
                });
                row.col(|ui| {
                    ui.label(p.status);
                });
                row.col(|ui| {
                    ui.label(&p.user);
                });
                row.col(|ui| {
                    // 不用 Label::truncate()：文本被省略时它会自动挂一个"悬停看全文"的
                    // tooltip，是浮层，会挡在行上干扰点击。这里自己画省略后的文本，
                    // 完整路径在右键「详细」里看。
                    paint_elided(ui, &p.exe, ui.visuals().weak_text_color());
                });

                let resp = row.response();
                if resp.hovered() {
                    resp.ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                // 列宽拖拽手柄（resizable）会在每列边界占掉约 10px，
                // 且手柄只有 drag 感应，点在上面行收不到 clicked()。
                // 表现为"点在文字最左边没反应"。这里再用指针位置兜底：
                // 只要按下/抬起落在本行范围内，就当作点了这一行。
                let clicked_by_pos = resp.ctx.input(|i| {
                    (i.pointer.primary_pressed() || i.pointer.primary_released())
                        && i
                            .pointer
                            .interact_pos()
                            .is_some_and(|pos| resp.rect.contains(pos))
                });
                // 右键也先选中该行，让菜单作用对象和视觉高亮保持一致
                if resp.clicked() || clicked_by_pos || resp.secondary_clicked() {
                    action = Action::Select(p.pid);
                }
                if resp.double_clicked() && !p.exe.is_empty() {
                    action = Action::Reveal(p.exe.clone());
                }
                resp.context_menu(|ui| {
                    ui.set_min_width(160.0);
                    ui.label(RichText::new(format!("{}  (PID {})", p.name, p.pid)).weak());
                    ui.separator();
                    if ui.button("结束进程").clicked() {
                        set_action(&mut action, Action::AskKill(p.pid, p.name.clone()));
                        ui.close_menu();
                    }
                    if !p.exe.is_empty() && ui.button("打开文件位置").clicked() {
                        set_action(&mut action, Action::Reveal(p.exe.clone()));
                        ui.close_menu();
                    }
                    if ui.button("复制 PID").clicked() {
                        ui.ctx().copy_text(p.pid.to_string());
                        ui.close_menu();
                    }
                    ui.separator();
                    ui.menu_button("详细", |ui| {
                        ui.set_min_width(380.0);
                        // 上界同样要给，否则超长命令行会把菜单撑到屏幕外
                        ui.set_max_width(460.0);
                        ui.label(
                            RichText::new(format!("{}（PID {}）", p.name, p.pid)).strong(),
                        );
                        ui.separator();
                        for (k, v) in proc_details(p) {
                            // 路径和命令行可能很长，单独占一行并自动换行
                            if k == "路径" || k == "命令行" {
                                ui.label(RichText::new(format!("{k}：")).weak());
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&v).monospace().small(),
                                    )
                                    .wrap(),
                                );
                            } else {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("{k}：")).weak());
                                    ui.label(RichText::new(&v).monospace());
                                });
                            }
                        }
                        ui.separator();
                        if !p.cmd.is_empty() && ui.button("复制命令行").clicked() {
                            ui.ctx().copy_text(p.cmd.clone());
                            ui.close_menu();
                        }
                        if ui.button("复制全部详细信息").clicked() {
                            let text = proc_details(p)
                                .into_iter()
                                .map(|(k, v)| format!("{k}：{v}"))
                                .collect::<Vec<_>>()
                                .join("\n");
                            ui.ctx().copy_text(text);
                            ui.close_menu();
                        }
                    });
                });
            });
        });

    action
}

fn draw_ports(
    ui: &mut egui::Ui,
    snap: &Snapshot,
    search: &str,
    listen_only: bool,
    sel_port: Option<&(String, u16)>,
) -> Action {
    let mut action = Action::None;
    let q = search.trim().to_lowercase();

    let mut rows: Vec<&PortInfo> = filter_ports(&snap.ports, &q, listen_only);

    // 端口查询模式下，命中的端口排在前面，方便一眼看到结果
    if !q.is_empty() && q.chars().all(|c| c.is_ascii_digit()) {
        rows.sort_by_key(|p| (!p.port.to_string().starts_with(&q), p.port));
    }

    ui.spacing_mut().item_spacing.x = 0.0;

    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .sense(egui::Sense::click())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::exact(72.0))
        .column(Column::exact(72.0))
        .column(Column::initial(200.0).at_least(90.0).clip(true))
        .column(Column::initial(200.0).at_least(80.0).clip(true))
        .column(Column::exact(100.0))
        .column(Column::exact(76.0))
        .column(Column::remainder().at_least(120.0).clip(true))
        .header(24.0, |mut h| {
            h.col(|ui| {
                ui.label(RichText::new("端口").strong());
            });
            h.col(|ui| {
                ui.label(RichText::new("协议").strong());
            });
            h.col(|ui| {
                ui.label(RichText::new("本地地址").strong());
            });
            h.col(|ui| {
                ui.label(RichText::new("远端地址").strong());
            });
            h.col(|ui| {
                ui.label(RichText::new("状态").strong());
            });
            h.col(|ui| {
                ui.label(RichText::new("PID").strong());
            });
            h.col(|ui| {
                ui.label(RichText::new("进程").strong());
            });
        })
        .body(|body| {
            body.rows(22.0, rows.len(), |mut row| {
                let p = rows[row.index()];
                row.set_selected(matches!(
                    sel_port,
                    Some((l, po)) if *po == p.port && l == &p.local
                ));

                row.col(|ui| {
                    ui.label(RichText::new(p.port.to_string()).monospace().strong());
                });
                row.col(|ui| {
                    ui.label(p.proto);
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(RichText::new(&p.local).monospace()).truncate())
                        .on_hover_text(&p.local);
                });
                row.col(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&p.remote).weak().monospace()).truncate(),
                    )
                    .on_hover_text(&p.remote);
                });
                row.col(|ui| {
                    let color = if p.state == "监听" {
                        Color32::from_rgb(0x2E, 0x7D, 0x32)
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.label(RichText::new(p.state).color(color));
                });
                row.col(|ui| {
                    match p.pid {
                        Some(pid) => {
                            ui.monospace(pid.to_string());
                        }
                        None => {
                            ui.label(RichText::new("-").weak());
                        }
                    };
                });
                row.col(|ui| {
                    ui.label(&p.pname);
                });

                let resp = row.response();
                if resp.hovered() {
                    resp.ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                // 同进程页：列宽拖拽手柄会吃掉列边界上的点击，用指针位置兜底
                let clicked_by_pos = resp.ctx.input(|i| {
                    (i.pointer.primary_pressed() || i.pointer.primary_released())
                        && i
                            .pointer
                            .interact_pos()
                            .is_some_and(|pos| resp.rect.contains(pos))
                });
                if resp.clicked() || clicked_by_pos || resp.secondary_clicked() {
                    action = Action::SelectPort(p.local.clone(), p.port, p.pid);
                }
                if resp.double_clicked() {
                    if let Some(pid) = p.pid {
                        action = Action::AskKill(pid, p.pname.clone());
                    }
                }
                resp.context_menu(|ui| {
                    ui.set_min_width(170.0);
                    ui.label(
                        RichText::new(format!("{}:{}  {}", p.proto, p.port, p.pname)).weak(),
                    );
                    ui.separator();
                    match p.pid {
                        Some(pid) => {
                            if ui.button("结束占用进程").clicked() {
                                set_action(&mut action, Action::AskKill(pid, p.pname.clone()));
                                ui.close_menu();
                            }
                            if ui.button("复制 PID").clicked() {
                                ui.ctx().copy_text(pid.to_string());
                                ui.close_menu();
                            }
                        }
                        None => {
                            ui.label(RichText::new("系统保留端口，无关联进程").weak());
                        }
                    }
                });
            });
        });

    action
}

fn sort_header(ui: &mut egui::Ui, text: &str, active: bool, asc: bool) -> egui::Response {
    let label = if active {
        format!("{text} {}", if asc { "▲" } else { "▼" })
    } else {
        text.to_string()
    };
    let color = if active {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    ui.add(egui::Button::new(RichText::new(label).strong().color(color)).frame(false))
}

/// 单行省略地画一段文本。
///
/// 不用 `Label::truncate()` 的原因是：文本被省略时 egui 会自动给 label 挂一个
/// "悬停看全文"的 tooltip，而 tooltip 是独立图层，浮在行上方，既挡视线也可能
/// 让点击落空。完整内容已经在右键「详细」里，行内不再需要浮层提示。
fn paint_elided(ui: &mut egui::Ui, text: &str, color: Color32) {
    if text.is_empty() {
        return;
    }
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::text::TextFormat {
            font_id,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: ui.available_width(),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.fonts(|f| f.layout_job(job));
    let (rect, _) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    ui.painter().galley(rect.left_top(), galley, color);
}

/// 右键「详细」里展示的字段；悬停提示只取其中几项，信息源统一在这里
fn proc_details(p: &ProcInfo) -> Vec<(&'static str, String)> {
    let mut v = vec![
        ("PID", p.pid.to_string()),
        ("状态", p.status.to_string()),
        ("用户", p.user.clone()),
        ("内存", fmt_mem(p.mem)),
        ("CPU", format!("{:.1}%", p.cpu)),
    ];
    if let Some(ppid) = p.ppid {
        v.push(("父进程 PID", ppid.to_string()));
    }
    if !p.exe.is_empty() {
        v.push(("路径", p.exe.clone()));
    }
    if !p.cmd.is_empty() {
        v.push(("命令行", p.cmd.clone()));
    }
    v
}

fn fmt_mem(bytes: u64) -> String {
    let mb = bytes as f64 / 1024.0 / 1024.0;
    if mb >= 1024.0 {
        format!("{:.2} GB", mb / 1024.0)
    } else {
        format!("{mb:.1} MB")
    }
}

fn elapsed_cn(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0 => "刚刚".to_string(),
        1..=59 => format!("{s} 秒前"),
        60..=3599 => format!("{} 分钟前", s / 60),
        _ => format!("{} 小时前", s / 3600),
    }
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // egui 自带字体不含中日韩字形，必须挂载系统中文字体，否则界面全是方块
    let candidates = [
        ("C:/Windows/Fonts/msyh.ttc", "msyh"),
        ("C:/Windows/Fonts/simhei.ttf", "simhei"),
        ("C:/Windows/Fonts/Deng.ttf", "deng"),
    ];

    let mut loaded = None;
    for (path, name) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.to_owned(), egui::FontData::from_owned(bytes));
            loaded = Some(name.to_owned());
            break;
        }
    }

    if let Some(name) = loaded {
        if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            f.insert(0, name.clone());
        }
        // 等宽族里中文排在后面：数字英文仍用等宽体，中文字符回落到雅黑
        if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            f.push(name);
        }
    }

    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::light());
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::proportional(13.5),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::proportional(13.5),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        egui::FontId::monospace(13.0),
    );
    style.visuals.panel_fill = Color32::from_rgb(0xFA, 0xFA, 0xFB);
    // egui 默认允许选中 Label 文本，为此给每个 label 挂的是 click_and_drag 感应，
    // 结果表格里"点文字"会被 label 吃掉，行本身收不到点击（只有点空白处才选得中）。
    // 进程列表不需要选中文本，关掉它，让点击统一落到行上。
    style.interaction.selectable_labels = false;
    // 选中行要高亮得足够明显：表格库用 selection.bg_fill 铺底、
    // 用 selection.stroke.color 覆盖文字色，所以这两个都要调。
    style.visuals.selection.bg_fill = Color32::from_rgb(0xB3, 0xD4, 0xFF);
    style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(0x0D, 0x2E, 0x66));
    ctx.set_style(style);
}

#[cfg(test)]
mod tests {
    //! 表格行的交互回归测试。
    //!
    //! `TableBuilder` 的默认 `sense` 是 `Sense::hover()`，一旦有人漏掉
    //! `.sense(Sense::click())`，行就收不到任何点击落在鼠标上：左键选中失效、
    //! 右键菜单永远不弹。这两个用例直接驱动 `egui::Context` 模拟真实指针事件，
    //! 把这条链路钉死。

    use super::*;

    fn fake_snapshot() -> Snapshot {
        let mk = |pid: u32, name: &str| ProcInfo {
            pid,
            ppid: Some(4),
            name: name.to_string(),
            cpu: 1.5,
            mem: 8 * 1024 * 1024,
            status: "运行中",
            user: "tester".to_string(),
            exe: "C:/Windows/System32/DriverStore/FileRepository/amdkmdag_2/service/proc.exe".to_string(),
            cmd: "proc.exe --demo".to_string(),
        };
        Snapshot {
            procs: vec![mk(1001, "alpha.exe"), mk(1002, "beta.exe")],
            ports: vec![],
            at: Instant::now(),
            cost_ms: 0,
            error: None,
        }
    }

    fn base_raw() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 600.0),
            )),
            ..Default::default()
        }
    }

    /// 测试必须复用 App 启动时的那套样式（字号、间距、selectable_labels 等），
    /// 否则测的是 egui 默认样式，跟真实界面不是一回事。
    fn test_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        setup_style(&ctx);
        ctx
    }

    /// 跑一帧，返回这一帧里表格产生的动作
    fn frame(ctx: &egui::Context, snap: &Snapshot, events: Vec<egui::Event>) -> Action {
        let mut raw = base_raw();
        raw.events = events;
        let mut out = Action::None;
        ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                out = draw_procs(ui, snap, "", (SortKey::Pid, true), None);
            });
        });
        out
    }

    fn press(pos: egui::Pos2, button: egui::PointerButton, pressed: bool) -> Vec<egui::Event> {
        // 每帧都要带上 PointerMoved，否则指针位置会被重置
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::default(),
            },
        ]
    }

    /// egui 的点击判定要求"按下"和"抬起"分处不同帧，同帧收发不算点击。
    /// 所以在 pos 处完整走一遍：悬停 → 按下 → 抬起。
    fn click_at(
        ctx: &egui::Context,
        snap: &Snapshot,
        pos: egui::Pos2,
        button: egui::PointerButton,
    ) -> Action {
        frame(ctx, snap, vec![egui::Event::PointerMoved(pos)]);
        frame(ctx, snap, press(pos, button, true));
        frame(ctx, snap, press(pos, button, false))
    }

    /// 表头 24px + 行高 22px，从 34 起往下扫，第一处命中的就是行
    const PROBE_X: f32 = 120.0;
    const PROBE_YS: [f32; 9] = [34.0, 44.0, 54.0, 64.0, 74.0, 84.0, 94.0, 104.0, 124.0];

    fn scan_for_click(button: egui::PointerButton) -> Option<(f32, Action, egui::Context)> {
        let snap = fake_snapshot();
        scan_in(&snap, button)
    }

    fn scan_in(
        snap: &Snapshot,
        button: egui::PointerButton,
    ) -> Option<(f32, Action, egui::Context)> {
        for y in PROBE_YS {
            let ctx = test_ctx();
            ctx.run(base_raw(), |_| {}); // 首帧只为完成布局
            let a = click_at(&ctx, snap, egui::pos2(PROBE_X, y), button);
            if !matches!(a, Action::None) {
                return Some((y, a, ctx));
            }
        }
        None
    }

    #[test]
    fn left_click_selects_row() {
        let (y, a, _ctx) = scan_for_click(egui::PointerButton::Primary)
            .expect("left click on any row should yield Action::Select, got none");
        match a {
            Action::Select(pid) => assert!(
                pid == 1001 || pid == 1002,
                "selected pid {pid} is not from the fixture (y={y})"
            ),
            other => panic!("expected Action::Select, got {}", action_name(&other)),
        }
    }

    #[test]
    fn right_click_opens_context_menu() {
        let (y, a, ctx) = scan_for_click(egui::PointerButton::Secondary)
            .expect("right click on any row should yield Action::Select, got none");
        assert!(
            matches!(a, Action::Select(_)),
            "right click produced {} instead of Action::Select (y={y})",
            action_name(&a)
        );
        // egui 0.29 的新菜单体系把状态存在 Context 的 data 里，不走 Memory::popup，
        // 所以不能用 any_popup_open 判断；改成看有没有多出一层可见的浮层（菜单是独立 Area）。
        let extra_layers = ctx.memory(|m| {
            m.areas()
                .visible_layer_ids()
                .iter()
                .filter(|l| **l != egui::LayerId::background())
                .count()
        });
        assert!(
            extra_layers > 0,
            "row received the secondary click at y={y} but no popup layer was created"
        );
    }

    #[test]
    fn ports_row_click_selects_and_opens_menu() {
        let snap = Snapshot {
            procs: vec![],
            ports: vec![
                PortInfo {
                    proto: "TCP",
                    local: "0.0.0.0:445".to_string(),
                    port: 445,
                    remote: "0.0.0.0:0".to_string(),
                    state: "监听",
                    pid: Some(4),
                    pname: "System".to_string(),
                },
                PortInfo {
                    proto: "TCP",
                    local: "0.0.0.0:5353".to_string(),
                    port: 5353,
                    remote: "0.0.0.0:0".to_string(),
                    state: "监听",
                    pid: None,
                    pname: "-".to_string(),
                },
            ],
            at: Instant::now(),
            cost_ms: 0,
            error: None,
        };

        let click = |button: egui::PointerButton, y: f32| {
            let ctx = test_ctx();
            ctx.run(base_raw(), |_| {});
            let mut out = Action::None;
            let mut run = |events: Vec<egui::Event>| {
                let mut raw = base_raw();
                raw.events = events;
                ctx.run(raw, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        out = draw_ports(ui, &snap, "", false, None);
                    });
                });
            };
            let pos = egui::pos2(PROBE_X, y);
            run(vec![egui::Event::PointerMoved(pos)]);
            run(press(pos, button, true));
            run(press(pos, button, false));
            (out, ctx)
        };

        // 不硬编码行高：逐行扫一遍，记录点到了哪些端口
        let mut seen: Vec<(u16, Option<u32>)> = Vec::new();
        for y in PROBE_YS {
            let (a, _ctx) = click(egui::PointerButton::Primary, y);
            if let Action::SelectPort(_, port, pid) = a {
                if !seen.iter().any(|(p, _)| *p == port) {
                    seen.push((port, pid));
                }
            }
        }

        assert!(
            seen.iter().any(|(p, pid)| *p == 445 && *pid == Some(4)),
            "port 445 row should be clickable and carry its pid, got {seen:?}"
        );
        // 旧实现用 `selected == Some(pid)` 判断高亮，PID 为空的行永远选不中
        assert!(
            seen.iter().any(|(p, pid)| *p == 5353 && pid.is_none()),
            "pid-less row (5353) should also be selectable, got {seen:?}"
        );
    }

    #[test]
    fn any_point_in_the_row_selects_it() {
        // 这条用例同时守住三个坑：
        // 1. TableBuilder 默认 Sense::hover() —— 行根本收不到点击；
        // 2. selectable_labels=true —— label 会把文字上的点击吃掉；
        // 3. 列之间的 item_spacing 缝隙不属于任何单元格，点上去毫无反应。
        // 所以横向上逐像素扫一整行，任何位置都必须能选中。
        let snap = fake_snapshot();
        let (y, _, _) = scan_in(&snap, egui::PointerButton::Primary)
            .expect("row should be clickable at all");

        let mut failed = Vec::new();
        let mut x = 10.0_f32; // 左边界外是面板留白，不算行内
        while x < 880.0 {
            let ctx = test_ctx();
            ctx.run(base_raw(), |_| {});
            let a = click_at(&ctx, &snap, egui::pos2(x, y), egui::PointerButton::Primary);
            if !matches!(a, Action::Select(_)) {
                failed.push(format!("x={x} -> {}", action_name(&a)));
            }
            x += 4.0; // 死区约 10px 宽，4px 步长足够捕获
        }
        assert!(
            failed.is_empty(),
            "these points in the row did not select it: {failed:?}"
        );
    }

    #[test]
    fn hover_does_not_break_the_click() {
        // 名称列曾挂 on_hover_text、路径列用 Label::truncate() 会自动弹 tooltip，
        // tooltip 是独立图层，悬停时浮在行上方。这里推进时间让浮层真的出现，
        // 再点击，确认仍然能选中（行内已不再有浮层提示）。
        let snap = fake_snapshot();
        let (y, _, _) = scan_in(&snap, egui::PointerButton::Primary).expect("row");

        for px in [200.0_f32, 400.0, 700.0, 800.0] {
            let pos = egui::pos2(px, y);
            let ctx = test_ctx();
            let mut t = 0.0_f64;
            frame_at(&ctx, &snap, vec![egui::Event::PointerMoved(pos)], t);
            for _ in 0..12 {
                t += 0.1;
                frame_at(&ctx, &snap, vec![], t);
            }
            let layers = ctx.memory(|m| {
                m.areas()
                    .visible_layer_ids()
                    .iter()
                    .filter(|l| **l != egui::LayerId::background())
                    .count()
            });
            frame_at(&ctx, &snap, press(pos, egui::PointerButton::Primary, true), t);
            let a = frame_at(&ctx, &snap, press(pos, egui::PointerButton::Primary, false), t);
            assert!(
                matches!(a, Action::Select(_)),
                "click at ({px}, {y}) failed after hovering; popup layers={layers}"
            );
        }
    }

    /// 推进时间的帧，用于让 tooltip 这类带延迟的东西真正出现
    fn frame_at(
        ctx: &egui::Context,
        snap: &Snapshot,
        events: Vec<egui::Event>,
        time: f64,
    ) -> Action {
        let mut raw = base_raw();
        raw.time = Some(time);
        raw.events = events;
        let mut out = Action::None;
        ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                out = draw_procs(ui, snap, "", (SortKey::Pid, true), None);
            });
        });
        out
    }


    fn action_name(a: &Action) -> &'static str {
        match a {
            Action::None => "None",
            Action::Select(_) => "Select",
            Action::SelectPort(..) => "SelectPort",
            Action::Sort(_) => "Sort",
            Action::AskKill(..) => "AskKill",
            Action::Reveal(_) => "Reveal",
            Action::Refresh => "Refresh",
        }
    }
}
