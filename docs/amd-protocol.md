# AMD 后台适配记录

本适配基于本机已安装的 `System32\AMD\ANR\AMDNoiseSuppression.exe` 的静态检查与本机请求/响应验证，不是 AMD 官方公开 SDK。

已验证 SHA-256：

```
4ecec1b76d9ef18c7f2a26da823e71124db627e3acd3c178c89b80b3aeac20ee
```

后台创建大小 `0x29e08` 的 `Local\AMDANRMemoryMap`，并使用 `Local\ANR_EVT_S` 请求事件、`Local\ANR_EVT_D` 完成事件。程序仅打开已有对象，绝不自行创建 AMD 映射。设备 ID 为 128 个 UTF-16 单元的定长缓冲。

| 请求 | 命令值 | 设备 ID | 布尔状态 | 返回码 | 响应时间标记 |
|---|---:|---:|---:|---:|---:|
| GET_ANS_STATE | 0x86 | 0x868 | 0x968 | 0x96c | 0x860 |
| SET_ANS_STATE | 0x87 | 0x978 | 0xa78 | 0xa7c | 0x970 |

请求命令 DWORD 位于 `0x818`。返回码 `0` 为成功。写入前清空相关返回码和时间标记；请求后检查返回设备、命令专属响应标记和返回码。完成事件为自动复位且与 Radeon Software 共用，可能被其他客户端先消费，因此使用 50 ms 有界等待同时检查响应字段，而非仅依赖完成事件或易变的全局命令值。设置完成后再次 GET 验证。AudioPin 的 UI、诊断工具、测试脚本通过 `Local\AudioPin.AmdIpc.v1` 互斥串行请求；AMD Radeon Software 不共享此锁，发生竞用时检测不一致并退避。

配置值位于 `HKCU\Software\AMD\CN\Audio`：

- `ANRToggleState`: REG_SZ `true`
- `ANRAudioInputUseDefault`: REG_SZ `false`
- `ANRAudioInputDevice`: 原始输入 endpoint ID

这些值只在偏离时写入。进程存活和注册表状态都不能单独证明麦克风降噪开启，所以守护使用后台 GET 读取实际开关。没有采集音频或验证算法效果。

启动时校验后台二进制 SHA-256；运行中检测文件时间/大小变化并重新校验。新增驱动支持必须先验证 ABI，再增加经过测试的版本配置。不能仅扩大接受哈希列表而不测试。

Windows 音频路由接口布局参考 EarTrumpet 公开的 COM 接口定义（本项目独立用 Rust 实现）：

- https://github.com/File-New-Project/EarTrumpet/blob/master/EarTrumpet/Interop/MMDeviceAPI/IPolicyConfig.cs
- https://github.com/File-New-Project/EarTrumpet/blob/master/EarTrumpet/Interop/MMDeviceAPI/IAudioPolicyConfigFactoryVariantFor21H2.cs
- https://github.com/File-New-Project/EarTrumpet/blob/master/EarTrumpet/Interop/MMDeviceAPI/IAudioPolicyConfigFactoryVariantForDownlevel.cs

`IPolicyConfig` 的 SetDefaultEndpoint 槽位 13；`IAudioPolicyConfigFactory` 的 Set/GetPersistedDefaultAudioEndpoint 槽位 25/26（包括 IInspectable 的 6 槽）。调用 flow=1（capture），分别处理 console、multimedia、communications 三角色；写入后逐个复查。
