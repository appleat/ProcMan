// Windows 平台操作：结束进程、在资源管理器中定位文件

use std::os::windows::process::CommandExt;

/// 结束指定 PID 的进程（TerminateProcess，不做优雅退出协商）
pub fn kill_process(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return Err(err_text(GetLastError(), pid));
        }
        let ok = TerminateProcess(handle, 1);
        let code = GetLastError();
        CloseHandle(handle);
        if ok == 0 {
            Err(err_text(code, pid))
        } else {
            Ok(())
        }
    }
}

fn err_text(code: u32, pid: u32) -> String {
    match code {
        5 => "权限不足：该进程需要管理员权限，请以管理员身份重新运行本程序".to_string(),
        87 => format!("进程 {pid} 不存在（可能已经退出）"),
        6 => format!("进程 {pid} 的句柄无效"),
        _ => format!("操作失败（Windows 错误码 {code}）"),
    }
}

/// 在资源管理器中选中该文件
pub fn reveal_in_explorer(path: &str) {
    if path.is_empty() {
        return;
    }
    let _ = std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{path}\""))
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn kill_really_terminates_process() {
        let mut child = std::process::Command::new("C:\\Windows\\System32\\ping.exe")
            .args(["-n", "60", "127.0.0.1"])
            .creation_flags(0x0800_0000)
            .spawn()
            .expect("启动子进程失败");
        let pid = child.id();

        std::thread::sleep(Duration::from_millis(400));
        assert!(
            child.try_wait().unwrap().is_none(),
            "子进程本应仍在运行"
        );

        kill_process(pid).expect("结束子进程应当成功");
        std::thread::sleep(Duration::from_millis(400));

        assert!(
            child.try_wait().unwrap().is_some(),
            "结束进程后子进程应当已退出"
        );
    }

    #[test]
    fn kill_missing_pid_reports_error() {
        // 0 号 PID 永远不存在，用来验证错误分支不会 panic
        let err = kill_process(0).unwrap_err();
        assert!(!err.is_empty());
    }
}
