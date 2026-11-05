# AudioPin

Rust 编写的 Windows 原生音频守护工具，重点解决 AMD 麦克风降噪意外关闭、远程音频设备抢占默认输入、麦克风断连后配置漂移。

## 功能

- Win32 原生控件，五个页面、系统托盘、单实例、DPI 缩放、键盘导航；无 WebView。
- 查询 AMD 后台的**麦克风运行开关**，关闭后恢复并再次读取验证。同时固定 AMD 的原始输入配置。
- 设备按完整 ID 固定。离线时等待原设备重新出现；不把同名或远程虚拟麦克风当成原设备。
- AMD 后台退出时可重新启动；失败指数退避，设备变化时立即重置退避；间隔 1–60 秒可配。
- 固定 Windows 默认输入、默认通信输入；为指定 `.exe` 固定应用输入路由。
- 状态变化通知、最近 200 条事件、1 MiB 日志轮转、诊断导出。
- 登录自启、启动到托盘、关闭到托盘、暂停/恢复、立即检查、配置重新载入。
- 关于页与 GitHub Release 检查；私有仓库使用本机 `gh` 登录，无需将令牌保存到配置。

## 安装与设置

1. 从本仓库 Releases 下载 `AudioPin-windows-x64.zip` 并解压。双击 `AudioPin.exe` 可以直接使用。
2. 需要安装和自启时，在解压目录执行 `powershell -ExecutionPolicy Bypass -File .\Install.ps1`。安装到 `%LOCALAPPDATA%\Programs\AudioPin`，不需要管理员权限。
3. 在“设备与路由”选择 **AMD 使用的原始麦克风**，例如 USB 麦克风；不要选 AMD Streaming。
4. 提供给 Windows / 语音软件的输入选择 **AMD Streaming Audio Device**。按需勾选“固定默认输入”和“固定通信输入”。
5. 在“语音软件”填写进程名，例如 `YY.exe`、`KOOK.exe`，启用应用守护；名称须与任务管理器“详细信息”一致。多进程软件需填写实际建立音频流的进程名。
6. 在 YY / KOOK 中选择“系统默认”或“默认通信设备”。已打开的语音流可能需要重新进入频道才能使用新路由。
7. 点击“保存并应用”。关闭窗口后默认继续在托盘守护；托盘右键可暂停或彻底退出。

第一次运行会读取 AMD 现有原始输入；**不会自动开启 Windows/应用路由固定**，需要选定输出设备后开启。安装脚本默认添加自启，UI 可随时取消。

## 支持范围与边界

- Windows 10/11 x64，当前用户会话。无后台服务、无录音、无音频上传、无遥测。
- **AMD 适配当前只开放经过本机验证的后台二进制**。SHA-256 见 [协议适配说明](docs/amd-protocol.md)。驱动升级为其他版本时停止私有协议写入并提示；不会试用猜测的内存布局。Windows 路由能力可在关闭 AMD 守护后单独使用。
- “正在守护”验证 AMD 运行开关与音频设备连接，不能证明麦克风有声音，也不评估降噪算法效果。
- Windows 应用路由不能覆盖软件内部明确指定另一设备的行为，也不保证即时切换已打开的音频流。YY/KOOK 使用系统默认设备时适用；本版本没有修改它们的私有配置文件。
- AMD 协议与 Windows 应用路由均是版本相关的内部接口，遇到不支持/访问失败会显式报告并退避。
- 暂停守护不会主动关闭降噪。它会尽可能还原**本次运行中**修改过、且仍保持 AudioPin 所设值的 Windows/应用路由；不会覆盖用户后续手动改动。退出时保留最后音频设置。
- 托盘通知受 Windows“勿扰”与通知策略影响；无论是否弹窗，状态与日志仍记录。
- 更新检查不自动替换 EXE。点击发布页下载更新，退出旧版本后安装。未登录、断网、无 Release 等情况会显示错误，不会报成“已是最新”。

## 配置与诊断

配置：`%LOCALAPPDATA%\AudioPin\config.toml`。完整字段见 [config.example.toml](config.example.toml)。支持 UI 修改与 TOML 编辑后重新载入；未知字段、非法范围或潜在输入回路会被拒绝，旧配置保留。保存采用临时文件加原子替换。

日志：同目录下 `events.log`、`events.previous.log`。诊断文件 `diagnostics.json` 包含设备名称/ID、本机默认输入和适配状态，仅保存在本机，请自行决定是否分享。

命令行：

```powershell
.\AudioPin.exe --background       # 按设置启动到托盘
.\AudioPin.exe --diagnose         # 输出并写入 diagnostics.json
.\AudioPin.exe --check-update     # 检查并写入 update-check.txt
.\AudioPin.exe --self-test-routing # 对自身 PID 测试应用路由并还原；默认设备幂等写入测试
```

Release 为 Windows GUI 子系统；PowerShell 直接启动可能不等待。自动化使用 `Start-Process -Wait`，CLI 结果同时写入配置目录。开发版可直接查看控制台输出。

卸载：执行安装目录的 `Uninstall.ps1`。只移除程序、自启和开始菜单快捷方式，保留配置和日志。

## 开发

推荐 Rust stable MSVC 工具链与 Visual Studio C++ Build Tools / Windows SDK。在 Developer PowerShell 中运行：

```powershell
cargo +stable-x86_64-pc-windows-msvc fmt --check
cargo +stable-x86_64-pc-windows-msvc clippy --locked --all-targets -- -D warnings
cargo +stable-x86_64-pc-windows-msvc test --locked
cargo +stable-x86_64-pc-windows-msvc build --locked --release
```

`.cargo/config.toml` 静态链接 MSVC 运行库。`scripts/dev.ps1` 包含原开发机 E: 盘旧 SDK 的适配，其他环境可直接使用上述标准命令。GitHub Actions 在 Windows 上执行格式、Clippy、测试、Release 构建；`v*` 标签自动发布 ZIP 和 SHA-256。

模块：`amd.rs`（版本校验与 IPC）、`audio.rs`（Windows 设备/路由）、`guard.rs`（守护状态、退避与日志）、`config.rs`（校验与持久化）、`ui.rs`（Win32 界面/托盘）、`updates.rs`（GitHub 检查）。

真实 AMD 恢复验证：先启动 AudioPin，再执行 `python scripts/probe_amd.py --test-watchdog`。该命令会短暂关闭麦克风降噪、断言关闭、等待守护恢复，最后保证重新开启；只应在可接受短暂音频影响的本机测试会话执行。`python scripts/probe_amd.py` 只查询运行开关。

MIT License。AMD 商标与驱动归 AMD 所有，本项目不附带 AMD 二进制。
