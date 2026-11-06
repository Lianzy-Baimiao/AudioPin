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
| START_ANS（输入通道） | 0x84 | 选择器 0x848 = 0 | 无 | 0x84c | 0x840 |
| GET_ANS_STATE | 0x86 | 0x868（输入参数） | 0x968 | 0x96c | 0x860 |
| SET_ANS_STATE | 0x87 | 0x978 | 0xa78 | 0xa7c | 0x970 |
| SET_ANS_PROC_DEVICE | 0x89 | 处理器索引 0xa98 | 无 | 0xa9c | 0xa90 |
| USE_DEFAULT_INPUT | 0x8b | 无 | 0xbb8 = false | 0xbbc | 0xbb0 |

请求命令 DWORD 位于 `0x818`。返回码 `0` 为成功；仅 START_ANS_PROCESS 还接受 `1`（已初始化）。GET/SET 返回 `-2` 表示引擎尚未初始化。恢复流程为：初始化、设置现有处理器选择、取消跟随默认输入、启动输入通道、SET 指定麦克风开启，然后在后续周期核对 GET 和活动采集。

**GET 的设备 ID 是输入参数。** 本机 `ANRonnxLib.dll` 的实现比较该 ID 与引擎所持有的设备 ID 后返回开关。`ANR-bgproc-Lib.dll` 的同名包装函数却在响应后把映射中的 ID 复制给调用者，不能据此推断后台会返回“当前设备”。本轮用后台反汇编和不同输入状态的实际请求核对了该差异，保留按目标设备查询，不能查询上一次请求遗留的 ID。

请求前清空命令专属返回码和时间标记，完成后复查设备参数与返回码。`ANR_EVT_D` 为共用自动复位事件：阻塞等待可能取走 Radeon 的通知；完全不消费自己的完成通知，又会使 Radeon 的下一次等待提前返回。v0.1.3 每 25 ms 检查命令专属返回字段，最长 5 秒；只有命令仍匹配、时间标记和返回码均完成时，才以 **0 毫秒等待**消费该次通知，不重置事件、不等待未来通知。AudioPin 的 UI、诊断工具和测试脚本通过 `Local\AudioPin.AmdIpc.v1` 串行请求。Radeon 不共享这个锁，单槽协议仍无通用的多客户端事务保证；命令或参数不一致、超时都会报错并退避。面板重载期间，守护只被动读取初始化回执，不发送 AMD IPC 请求。

配置值位于 `HKCU\Software\AMD\CN\Audio`：

- `ANRToggleState`: REG_SZ `true`
- `ANRAudioInputUseDefault`: REG_SZ `false`
- `ANRAudioInputDevice`: 原始输入 endpoint ID

这些值只在偏离时写入。进程存活和注册表状态都不能单独证明麦克风降噪开启。自 v0.1.1 起，除后台 GET 外，还通过 Windows `IAudioSessionManager2` 查询目标物理输入上的活动会话，要求 PID 对应当前登录会话中安装路径匹配的 AMDNoiseSuppression.exe。v0.1.2 分别报告 AMD 与 Windows 路由状态：只有开关、物理输入配置和 AMD 活动采集均通过才累计 AMD 后台恢复次数；某项路由失败仍会显示总体异常。不创建录音流、不读取音频内容、不验证算法效果。开启命令成功后采集会话可能延迟出现，因此先报告“正在恢复”；长时间未出现则报错并退避重试。

后台自动启动前写入目标配置，使用安装目录作为工作目录，并传入 `NoQuit` 参数。启动和初始化请求后进入等待状态，避免在 AMD 自身异步建立采集期间重复 SET 设备。已静态确认本机 `ANR-bgproc-Lib.dll` 使用此参数；AMDNoiseSuppression.exe 在全局配置关闭且没有该参数时直接退出。本机通过 AMD 面板关闭总开关重现了旧实现的启动失败。

Radeon 的总开关与输入选择缓存在前端进程，单写注册表、后台 SET 或关闭再打开窗口不会更新这个模型。v0.1.3 默认在初次检查和每次后台恢复后调用 AMD 自带 `cncmd.exe restart`，由 AMD 协调退出和重启。直接结束 Radeon 再自行启动曾与它的 watch 进程产生竞争，因此未采用。仅重启又会进入 `atlogon` 模式，本机复现了降噪模块一直未初始化、面板音频区域缺失的情况。等待旧进程被替换后，再通过 ShellExecute 以 SW_HIDE 发出一次普通启动请求，触发 Radeon 自身的单实例初始化。

“重载完成”要求后台的初始化、通道启动、设备设置三项成功回执均比重载前更新，并在后续检查中持续满足至少 3 秒；45 秒未完成会报错并退避重试。START/SET 回执由输入和输出降噪共用，只证明初始化流程执行，不能独立证明原始麦克风已选对。因此下一周期还必须重新 GET 目标麦克风并确认 Windows 活动采集，才计入恢复成功。面板窗口会重启，AMD 可能自行显示窗口；`reload_amd_panel` 可关闭此行为。程序不读取 UI 控件，实际面板文字由本机 CUA 验收。

版本校验覆盖后台 EXE 和算法引擎 DLL（运行中监测时间/大小变化）；面板重载前另校验面板与命令行程序。新增版本必须先验证 ABI，不能只扩大哈希列表：

| 组件 | SHA-256 |
|---|---|
| AMDNoiseSuppression.exe | `4ecec1b76d9ef18c7f2a26da823e71124db627e3acd3c178c89b80b3aeac20ee` |
| ANRonnxLib.dll | `a34a6590a14a34d4a4b4155b63bc6357ed5654414ae3c04af0f791849179ac26` |
| RadeonSoftware.exe | `1d33caee9336ace73c6c15a56b5653d2516dcbcd94347dee9cd5d518f80a7243` |
| cncmd.exe | `27239a28e2f914b6d0a230abaa7631dc7f77a39d6d0e36eac2474cf0a4247064` |
| ANR-bgproc-Lib.dll | `d07610c76593e2cfb4d73644d879b87f5c2fdc6f7f0a0223c37d093b122b21d8` |

Windows 音频路由接口布局参考 EarTrumpet 公开的 COM 接口定义（本项目独立用 Rust 实现）：

- https://github.com/File-New-Project/EarTrumpet/blob/master/EarTrumpet/Interop/MMDeviceAPI/IPolicyConfig.cs
- https://github.com/File-New-Project/EarTrumpet/blob/master/EarTrumpet/Interop/MMDeviceAPI/IAudioPolicyConfigFactoryVariantFor21H2.cs
- https://github.com/File-New-Project/EarTrumpet/blob/master/EarTrumpet/Interop/MMDeviceAPI/IAudioPolicyConfigFactoryVariantForDownlevel.cs

`IPolicyConfig` 的 SetDefaultEndpoint 槽位 13；`IAudioPolicyConfigFactory` 的 Set/GetPersistedDefaultAudioEndpoint 槽位 25/26（包括 IInspectable 的 6 槽）。调用 flow=1（capture），分别处理 console、multimedia、communications 三角色；写入后逐个复查。
