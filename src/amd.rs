//! Version-gated AMD IPC adapter. See docs/amd-protocol.md before changing offsets.
use crate::platform::{handle, wide, Handle};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, ptr};
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::{Memory::*, Threading::*},
};
use winreg::{enums::*, RegKey};

const PROFILE_HASH: &str = "4ecec1b76d9ef18c7f2a26da823e71124db627e3acd3c178c89b80b3aeac20ee";
const ENGINE_HASH: &str = "a34a6590a14a34d4a4b4155b63bc6357ed5654414ae3c04af0f791849179ac26";
const MAP_SIZE: usize = 0x29e08;
const KEY: &str = r"Software\AMD\CN\Audio";
#[derive(Debug)]
struct BackendError(i32);
impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 == -2 {
            write!(f, "AMD 音频处理引擎尚未初始化（-2）")
        } else {
            write!(f, "AMD 返回错误 {}", self.0)
        }
    }
}
impl std::error::Error for BackendError {}
pub fn uninitialized(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<BackendError>()
        .is_some_and(|e| e.0 == -2)
}
pub fn executable() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
        .join(r"System32\AMD\ANR\AMDNoiseSuppression.exe")
}
pub fn supported() -> Result<()> {
    for (path, expected) in [
        (executable(), PROFILE_HASH),
        (executable().with_file_name("anronnxlib.dll"), ENGINE_HASH),
    ] {
        let bytes = std::fs::read(&path).context("未安装 AMD Noise Suppression")?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        if digest != expected {
            bail!(
                "此 AMD 组件版本尚未验证，已停止私有协议写入：{}，SHA-256: {digest}",
                path.display()
            );
        }
    }
    Ok(())
}
pub fn configured_input() -> String {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(KEY)
        .and_then(|k| k.get_value::<String, _>("ANRAudioInputDevice"))
        .unwrap_or_default()
        .trim_end_matches('\0')
        .to_string()
}
pub fn configured_on() -> Result<bool> {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY)?;
    Ok(key
        .get_value::<String, _>("ANRToggleState")?
        .trim_end_matches('\0')
        == "true")
}
pub fn input_is_pinned() -> Result<bool> {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY)?;
    Ok(key
        .get_value::<String, _>("ANRAudioInputUseDefault")?
        .trim_end_matches('\0')
        == "false")
}
pub fn configure(id: &str) -> Result<bool> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY)?;
    let mut changed = false;
    for (name, value) in [
        ("ANRToggleState", "true"),
        ("ANRAudioInputUseDefault", "false"),
        ("ANRAudioInputDevice", id),
    ] {
        let old: String = key.get_value(name).unwrap_or_default();
        if old.trim_end_matches('\0') != value {
            key.set_value(name, &value)?;
            changed = true;
        }
    }
    Ok(changed)
}
pub struct Amd {
    _mapping: Handle,
    start: Handle,
    done: Handle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}
/// A passive receipt of channel startup. Reading it sends no AMD command and
/// does not consume the completion event used by Radeon during initialization.
/// START/SET are shared with output suppression too; these receipts confirm
/// initialization, not the selected microphone. The next independent GET and
/// Windows capture-session observation must still validate the pinned input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputReceipt {
    initialized: u64,
    started: u64,
    selected: u64,
    ready: bool,
}
impl InputReceipt {
    pub fn confirms_reload(&self, before: &Self) -> bool {
        self.ready
            && self.initialized != 0
            && self.initialized != before.initialized
            && self.started != 0
            && self.started != before.started
            && self.selected != 0
            && self.selected != before.selected
    }
}
#[derive(Clone, Copy)]
enum Request<'a> {
    Initialize,
    Processing(u32),
    PinInput,
    StartInput,
    ReadDevice(&'a str),
    EnableDevice(&'a str),
}
impl Drop for Amd {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.view);
        }
    }
}
impl Amd {
    pub fn connect() -> Result<Self> {
        unsafe {
            let mapping = handle(OpenFileMappingW(
                FILE_MAP_READ | FILE_MAP_WRITE,
                0,
                wide(r"Local\AMDANRMemoryMap").as_ptr(),
            ))
            .context("AMD 后台未就绪")?;
            let start = handle(OpenEventW(
                EVENT_MODIFY_STATE | SYNCHRONIZE,
                0,
                wide(r"Local\ANR_EVT_S").as_ptr(),
            ))?;
            let done = handle(OpenEventW(
                SYNCHRONIZE,
                0,
                wide(r"Local\ANR_EVT_D").as_ptr(),
            ))?;
            let view = MapViewOfFile(mapping.0, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, MAP_SIZE);
            if view.Value.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(Self {
                _mapping: mapping,
                start,
                done,
                view,
            })
        }
    }
    pub fn input_receipt(&self) -> InputReceipt {
        unsafe {
            let p = self.view.Value.cast::<u8>();
            let read_i32 = |at| ptr::read_volatile(p.add(at).cast::<i32>());
            let read_u64 = |at| ptr::read_volatile(p.add(at).cast::<u64>());
            InputReceipt {
                initialized: read_u64(0x820),
                started: read_u64(0x840),
                selected: read_u64(0x970),
                ready: matches!(read_i32(0x828), 0 | 1)
                    && read_i32(0x84c) == 0
                    && read_i32(0xa7c) == 0,
            }
        }
    }
    fn request(&self, request: Request<'_>, current: impl Fn() -> bool) -> Result<bool> {
        let device = match request {
            Request::ReadDevice(id) | Request::EnableDevice(id) => Some(id),
            _ => None,
        };
        let id = device.map(wide);
        if let Some(device) = device {
            if id.as_ref().unwrap().len() > 128 || device.is_empty() || device.contains('\0') {
                bail!("AMD 设备 ID 超出协议范围");
            }
        }
        let (command, offset, result) = match request {
            Request::Initialize => (0x82u32, 0x828, 0x828),
            Request::StartInput => (0x84, 0x848, 0x84c),
            Request::ReadDevice(_) => (0x86, 0x868, 0x96c),
            Request::EnableDevice(_) => (0x87, 0x978, 0xa7c),
            Request::Processing(_) => (0x89, 0xa98, 0xa9c),
            Request::PinInput => (0x8b, 0xbb8, 0xbbc),
        };
        unsafe {
            let mutex = handle(CreateMutexW(
                ptr::null(),
                0,
                wide(r"Local\AudioPin.AmdIpc.v1").as_ptr(),
            ))?;
            let lock_deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
            let acquired = loop {
                if !current() {
                    bail!("守护设置已改变，AMD 请求已取消");
                }
                let result = WaitForSingleObject(mutex.0, 50);
                if result != windows_sys::Win32::Foundation::WAIT_TIMEOUT
                    || std::time::Instant::now() >= lock_deadline
                {
                    break result;
                }
            };
            if acquired != WAIT_OBJECT_0
                && acquired != windows_sys::Win32::Foundation::WAIT_ABANDONED
            {
                bail!("另一项 AudioPin 诊断正在访问 AMD，请稍后重试");
            }
            struct Lock(Handle);
            impl Drop for Lock {
                fn drop(&mut self) {
                    unsafe {
                        ReleaseMutex(self.0 .0);
                    }
                }
            }
            let _lock = Lock(mutex);
            if !current() {
                bail!("守护设置已改变，AMD 请求已取消");
            }
            let p = self.view.Value.cast::<u8>();
            // Only touch the verified command's fields. Never create or resize AMD's map.
            if let Some(id) = &id {
                ptr::write_bytes(p.add(offset), 0, 256);
                ptr::copy_nonoverlapping(id.as_ptr().cast::<u8>(), p.add(offset), id.len() * 2);
            }
            match request {
                Request::StartInput => ptr::write_volatile(p.add(offset).cast::<u32>(), 0),
                Request::Processing(index) => {
                    ptr::write_volatile(p.add(offset).cast::<u32>(), index)
                }
                Request::PinInput => ptr::write_volatile(p.add(offset), 0),
                Request::EnableDevice(_) => ptr::write_volatile(p.add(offset + 256), 1),
                _ => {}
            }
            ptr::write_volatile(p.add(result).cast::<i32>(), i32::MIN);
            ptr::write_volatile(p.add(offset - 8).cast::<u64>(), 0);
            ptr::write_volatile(p.add(0x818).cast::<u32>(), command);
            // Never reset the shared event before a request: it may belong to
            // Radeon. Validate command-specific fields before consuming ours.
            if SetEvent(self.start.0) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // A blocking event wait can steal Radeon's next response. Conversely,
            // leaving our event signaled makes its next wait return stale data.
            // Only acknowledge an already-completed, still-owned command with
            // a zero-time wait. Never wait on the event for a future response.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if ptr::read_volatile(p.add(0x818).cast::<u32>()) != command {
                    bail!("AMD 控制请求被其他客户端占用，将稍后重试");
                }
                let stamped = ptr::read_volatile(p.add(offset - 8).cast::<u64>()) != 0;
                let finished = ptr::read_volatile(p.add(result).cast::<i32>()) != i32::MIN;
                if stamped && finished && WaitForSingleObject(self.done.0, 0) == WAIT_OBJECT_0 {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    if ptr::read_volatile(p.add(0x818).cast::<u32>()) != command {
                        bail!("AMD 控制请求被其他客户端占用，将稍后重试");
                    }
                    bail!("AMD 后台响应超时，将退避后重试");
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            if let Some(id) = &id {
                let returned: Vec<_> = (0..id.len())
                    .map(|i| ptr::read_volatile(p.add(offset).cast::<u16>().add(i)))
                    .collect();
                if returned != *id {
                    bail!("AMD 响应设备不匹配，将稍后重试");
                }
            }
            let rc = ptr::read_volatile(p.add(result).cast::<i32>());
            // START_PROCESS returns 1 when already initialized. Other commands
            // still require 0; do not generalize all positive codes as success.
            if rc != 0 && !(command == 0x82 && rc == 1) {
                return Err(BackendError(rc).into());
            }
            Ok(device.is_none() || ptr::read_volatile(p.add(offset + 256)) == 1)
        }
    }
    pub fn state(&self, device: &str) -> Result<bool> {
        // ANRonnxLib compares this ID to its active endpoints. Despite the
        // frontend wrapper's misleading copy direction, the backend needs it
        // as INPUT; querying an arbitrary leftover map ID is not an observation.
        self.request(Request::ReadDevice(device), || true)
    }
    pub fn initialize(&self, current: impl Fn() -> bool) -> Result<()> {
        self.request(Request::Initialize, current).map(|_| ())
    }
    pub fn enable(&self, device: &str, current: impl Fn() -> bool) -> Result<()> {
        // Match Radeon's enable sequence. SET alone can leave the input channel
        // stopped after the user selected "noise suppression off".
        let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY)?;
        let processing = key.get_value::<u32, _>("ANRAudioProcessingLocation")?;
        if processing > 2 {
            bail!("AMD 处理器选择尚未验证：{processing}");
        }
        for request in [
            Request::Initialize,
            Request::Processing(processing),
            Request::PinInput,
            Request::StartInput,
            Request::EnableDevice(device),
        ] {
            self.request(request, &current)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarting_radeon_without_channel_initialization_is_not_success() {
        let before = InputReceipt {
            initialized: 10,
            started: 20,
            selected: 30,
            ready: true,
        };
        assert!(!before.confirms_reload(&before));
        let mut after = before.clone();
        after.initialized = 40;
        assert!(!after.confirms_reload(&before));
        after.started = 50;
        after.selected = 60;
        after.ready = false;
        assert!(!after.confirms_reload(&before));
        after.ready = true;
        assert!(after.confirms_reload(&before));
    }
}
