// 采集层：后台线程定期抓取进程快照与端口占用关系
//
// 之所以放到独立线程：sysinfo 全量刷新 + netstat 枚举在进程多时可达上百毫秒，
// 放在 UI 线程会导致掉帧。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use netstat2::{get_sockets_info, AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users};

#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    pub cpu: f32,
    pub mem: u64,
    pub status: &'static str,
    pub user: String,
    pub exe: String,
    pub cmd: String,
}

#[derive(Clone, Debug)]
pub struct PortInfo {
    pub proto: &'static str,
    pub local: String,
    pub port: u16,
    pub remote: String,
    pub state: &'static str,
    pub pid: Option<u32>,
    pub pname: String,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub procs: Vec<ProcInfo>,
    pub ports: Vec<PortInfo>,
    pub at: Instant,
    pub cost_ms: u128,
    pub error: Option<String>,
}

struct Collector {
    sys: System,
    users: Users,
}

impl Collector {
    fn refresh(&mut self) {
        // user / cmd / exe 这类信息每次重新取代价高，且基本不变，用 OnlyIfNotSet 只补一次；
        // CPU 与内存必须每次刷新才准。
        let kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_user(UpdateKind::OnlyIfNotSet);
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    }

    fn new() -> Self {
        let sys = System::new();
        // sysinfo 的 CPU 使用率是"两次采样之间的增量"，首次刷新拿到的是 0，
        // 所以这里预热一次，保证界面一打开就有真实数值。
        let mut me = Self {
            sys,
            users: Users::new_with_refreshed_list(),
        };
        me.refresh();
        std::thread::sleep(Duration::from_millis(260));
        me.refresh();
        me
    }

    fn collect(&mut self) -> Snapshot {
        let t0 = Instant::now();
        self.refresh();

        let mut procs = Vec::with_capacity(self.sys.processes().len());
        for (pid, p) in self.sys.processes() {
            let user = p
                .user_id()
                .and_then(|uid| self.users.get_user_by_id(uid))
                .map(|u| u.name().to_string())
                .unwrap_or_default();

            procs.push(ProcInfo {
                pid: pid.as_u32(),
                ppid: p.parent().map(|q| q.as_u32()),
                name: p.name().to_string_lossy().into_owned(),
                cpu: p.cpu_usage(),
                mem: p.memory(),
                status: status_cn(p.status()),
                user,
                exe: p.exe().map(|e| e.display().to_string()).unwrap_or_default(),
                cmd: p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" "),
            });
        }

        let (ports, error) = match query_ports(&procs) {
            Ok(v) => (v, None),
            Err(e) => (Vec::new(), Some(e)),
        };

        Snapshot {
            procs,
            ports,
            at: Instant::now(),
            cost_ms: t0.elapsed().as_millis(),
            error,
        }
    }
}

fn query_ports(procs: &[ProcInfo]) -> Result<Vec<PortInfo>, String> {
    let af = AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6;
    let proto = ProtocolFlags::TCP | ProtocolFlags::UDP;
    let sockets =
        get_sockets_info(af, proto).map_err(|e| format!("读取端口占用失败：{e}"))?;

    let names: HashMap<u32, String> = procs.iter().map(|p| (p.pid, p.name.clone())).collect();
    let mut out = Vec::with_capacity(sockets.len());

    for si in sockets {
        let pids = &si.associated_pids;
        let pid = pids.first().copied();
        let pname = match pid {
            Some(p) => names
                .get(&p)
                .cloned()
                .unwrap_or_else(|| "已退出".to_string()),
            None => "系统保留".to_string(),
        };
        let pname = if pids.len() > 1 {
            format!("{pname} (+{} 个)", pids.len() - 1)
        } else {
            pname
        };

        match si.protocol_socket_info {
            ProtocolSocketInfo::Tcp(t) => out.push(PortInfo {
                proto: if t.local_addr.is_ipv6() { "TCP6" } else { "TCP" },
                local: t.local_addr.to_string(),
                port: t.local_port,
                remote: if t.remote_port == 0 {
                    "-".to_string()
                } else {
                    format!("{}:{}", t.remote_addr, t.remote_port)
                },
                state: tcp_state_cn(t.state),
                pid,
                pname,
            }),
            ProtocolSocketInfo::Udp(u) => out.push(PortInfo {
                proto: if u.local_addr.is_ipv6() { "UDP6" } else { "UDP" },
                local: u.local_addr.to_string(),
                port: u.local_port,
                remote: "-".to_string(),
                state: "监听",
                pid,
                pname,
            }),
        }
    }

    out.sort_by(|a, b| a.port.cmp(&b.port).then(a.proto.cmp(b.proto)));
    Ok(out)
}

fn status_cn(s: sysinfo::ProcessStatus) -> &'static str {
    use sysinfo::ProcessStatus as S;
    match s {
        S::Run => "运行",
        S::Sleep => "休眠",
        S::Idle => "空闲",
        S::Stop => "停止",
        S::Zombie => "僵尸",
        S::Tracing => "跟踪",
        S::Dead => "已死",
        S::Wakekill => "唤醒终止",
        S::Waking => "唤醒中",
        S::Parked => "挂起",
        _ => "未知",
    }
}

fn tcp_state_cn(s: TcpState) -> &'static str {
    match s {
        TcpState::Listen => "监听",
        TcpState::Established => "已连接",
        TcpState::SynSent => "SYN_SENT",
        TcpState::SynReceived => "SYN_RECV",
        TcpState::FinWait1 => "FIN_WAIT1",
        TcpState::FinWait2 => "FIN_WAIT2",
        TcpState::CloseWait => "CLOSE_WAIT",
        TcpState::Closing => "CLOSING",
        TcpState::LastAck => "LAST_ACK",
        TcpState::TimeWait => "TIME_WAIT",
        TcpState::Closed => "已关闭",
        TcpState::DeleteTcb => "DELETE_TCB",
        TcpState::Unknown => "未知",
    }
}

/// 启动采集线程，返回（快照接收端，手动刷新触发端）
///
/// `interval_ms` 为 0 表示关闭自动刷新：线程只采集一次，之后阻塞等待手动触发。
pub fn spawn(interval_ms: Arc<AtomicU64>) -> (Receiver<Snapshot>, Sender<()>) {
    let (tx, rx) = channel();
    let (trig_tx, trig_rx) = channel::<()>();

    std::thread::Builder::new()
        .name("proc-collector".into())
        .spawn(move || {
            let mut c = Collector::new();
            loop {
                if tx.send(c.collect()).is_err() {
                    return;
                }
                let wait = interval_ms.load(Ordering::Relaxed);
                if wait == 0 {
                    if trig_rx.recv().is_err() {
                        return;
                    }
                } else {
                    match trig_rx.recv_timeout(Duration::from_millis(wait)) {
                        Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        })
        .expect("无法创建采集线程");

    (rx, trig_tx)
}
