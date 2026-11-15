# ProcMan

Windows 原生进程管理器，用 Rust + egui 写成。单文件可执行，无 WebView、无 Electron、无运行时依赖。

做它的起因很简单：任务管理器查「谁占用了这个端口」要翻两层界面，命令行又得记 `netstat -ano` 加 `tasklist`。ProcMan 把这两件事放在一个窗口里，输入端口号直接出进程。

![进程列表](screenshots/01-进程列表.png)

## 功能

| 能力 | 说明 |
|---|---|
| 进程列表 | PID、名称、CPU、内存、状态、用户、可执行文件路径，点表头可按 CPU / 内存 / PID / 名称排序 |
| 结束进程 | 二次确认后调用 `TerminateProcess`；失败时把 Windows 错误码翻译成可读原因（如权限不足），并禁止误杀自身 |
| 按端口查进程 | 枚举 TCP / UDP（含 IPv6），输入端口号直接定位占用进程；数字查询**精确匹配优先**，查 445 不会把 64459 带出来 |
| 右键菜单 | 结束进程、打开文件位置、复制 PID，以及「详细」子菜单（父进程、完整路径、完整命令行） |

其他细节：F5 立即刷新、Delete 结束选中项、1/2/3/5/10 秒自动刷新间隔、双击行打开文件位置、整行点击选中高亮。

![按端口查进程](screenshots/03-端口查询.png)

## 构建

需要 Rust 工具链。本项目在 Windows 上使用 GNU 目标，不依赖 Visual Studio：

```bash
rustup default stable-x86_64-pc-windows-gnu
cargo build --release
```

产物在 `target/release/procman.exe`。

> **Windows GNU 目标注意**：编译依赖 `dlltool`、`as`、`ld`。rustup 分发的 `self-contained` 目录里只有 `dlltool`，缺 `as.exe` 会报错。装上 binutils 并加入 PATH 即可（例如 MSYS2 的 `mingw-w64-x86_64-binutils`）。
>
> 仓库里的 `build.bat` 已经把工具链路径拼好，双击或命令行运行它也能直接编译。

## 实现说明

`src/` 下五个模块，约 1700 行：

| 文件 | 职责 |
|---|---|
| `main.rs` | 窗口创建、字体加载（中文字体回退链） |
| `app.rs` | 界面与交互：表格、搜索、排序、右键菜单、确认框、测试 |
| `data.rs` | 数据采集：独立线程定时快照，UI 只读，进程多时也不掉帧 |
| `winutil.rs` | Win32 调用：`TerminateProcess`、错误码翻译、资源管理器定位 |
| `shot.rs` | 调试用截图（`--features screenshot`，发布版不含） |

技术选型上，`eframe` / `egui` 提供原生 Win32 窗口加 OpenGL 直接绘制；进程数据用 `sysinfo`，端口用 `netstat2`（底层 `GetExtendedTcpTable`），结束进程走 `windows-sys`。

采集线程按固定间隔刷新并产出快照，主线程渲染时只读快照，避免遍历进程列表时阻塞 UI。空闲占用约 0.3% CPU、160 MB 内存。

## 已知限制

- 结束进程是强制终止（等价任务管理器的「结束任务」），不会给程序保存数据的机会。
- 处理受保护的系统进程（如 PID 4）会因权限失败，需以管理员身份运行。

## License

MIT
