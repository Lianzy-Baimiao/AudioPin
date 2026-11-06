# AMD 后台适配记录

本适配基于本机已安装的 `System32\AMD\ANR\AMDNoiseSuppression.exe` 的静态检查与本机请求/响应验证，不是 AMD 官方公开 SDK。

已验证 SHA-256：

```
4ecec1b76d9ef18c7f2a26da823e71124db627e3acd3c178c89b80b3aeac20ee
```

后台创建大小 `0x29e08` 的 `Local\AMDANRMemoryMap`，并使用 `Local\ANR_EVT_S` 请求事件、`Local\ANR_EVT_D` 完成事件。程序仅打开已有对象，绝不自行创建 AMD 映射。设备 ID 为 128 个 UTF-16 单元的定长缓冲。

| 请求 | 命令值 | 设备 ID | 布尔状态 | 返回码 | 响应时间标记 |
|---|---:|---:|---:|---:|---:|
| START_ANS_PROCESS | 0x82 | 无 | 无 | 0x828 | 0x820 |
| GET_ANS_STATE | 0x86 | 0x868 | 0x968 | 0x96c | 0x860 |
| SET_ANS_STATE | 0x87 | 0x978 | 0xa78 | 0xa7c | 0x970 |

请求命令 DWORD 位于 `0x818`。返回码 `0` 为成功；仅 START_ANS_PROCESS 还接受 `1`（已初始化）。GET/SET 返回 `-2` 表示引擎尚未初始化，不能解释成设备断开；守护发送初始化后重新 GET。写入前清空相关返回码和时间标记；请求后检查返回设备、命令专属响应标记和返回码。完成事件为自动复位且与 Radeon Software 共用，可能被其他客户端先消费，因此使用 50 ms 有界等待同时检查响应字段，而非仅依赖完成事件或易变的全局命令值。设置完成后再次 GET 验证。AudioPin 的 UI、诊断工具、测试脚本通过 `Local\AudioPin.AmdIpc.v1` 互斥串行请求；AMD Radeon Software 不共享此锁，发生竞用时检测不一致并退避。

配置值位于 `HKCU\Software\AMD\CN\Audio`：

- `ANRToggleState`: REG_SZ `true`
- `ANRAudioInputUseDefault`: REG_SZ `false`
- `ANRAudioInputDevice`: 原始输入 endpoint ID

这些值只在偏离时写入。进程存活和注册表状态都不能单独证明麦克风降噪开启。v0.1.1 除后台 GET 外，还通过 Windows `IAudioSessionManager2` 查询目标物理输入上的活动会话，要求 PID 对应 AMDNoiseSuppression.exe，且配置的路由检查成功，才累计恢复次数和通知成功。不创建录音流、不读取音频内容、不验证算法效果。开启命令成功后采集会话可能延迟出现，因此先报告“正在恢复”；长时间未出现则报错并退避重试。

后台自动启动前写入目标配置，使用安装目录作为工作目录，并传入 `NoQuit` 参数。启动和初始化请求后进入等待状态，避免在 AMD 自身异步建立采集期间重复 SET 设备。已静态确认本机 `ANR-bgproc-Lib.dll` 使用此参数；AMDNoiseSuppression.exe 在全局配置关闭且没有该参数时直接退出。本机通过 AMD 面板关闭总开关重现了旧实现的启动失败。

已观察到：通过后台 SET 恢复后 Windows 上 AMD 的物理输入会话重新活动，但 Radeon Software 的输入下拉框仍显示“噪音抑制关闭”；页面切换、关闭重开面板和重启面板进程均未解决。面板同步尚未实现，不可用后台开关或采集会话的成功冒充面板验证成功。

启动时校验后台二进制 SHA-256；运行中检测文件时间/大小变化并重新校验。新增驱动支持必须先验证 ABI，再增加经过测试的版本配置。不能仅扩大接受哈希列表而不测试。

Windows 音频路由接口布局参考 EarTrumpet 公开的 COM 接口定义（本项目独立用 Rust 实现）：

- https://github.com/File-New-Project/EarTrumpet/blob/master/EarTrumpet/Interop/MMDeviceAPI/IPolicyConfig.cs
- https://github.com/File-New-Project/EarTrumpet/blob/master/EarTrumpet/Interop/MMDeviceAPI/IAudioPolicyConfigFactoryVariantFor21H2.cs
- https://github.com/File-New-Project/EarTrumpet/blob/master/EarTrumpet/Interop/MMDeviceAPI/IAudioPolicyConfigFactoryVariantForDownlevel.cs

`IPolicyConfig` 的 SetDefaultEndpoint 槽位 13；`IAudioPolicyConfigFactory` 的 Set/GetPersistedDefaultAudioEndpoint 槽位 25/26（包括 IInspectable 的 6 槽）。调用 flow=1（capture），分别处理 console、multimedia、communications 三角色；写入后逐个复查。
