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
const MAP_SIZE: usize = 0x29e08;
const KEY: &str = r"Software\AMD\CN\Audio";
pub fn executable() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
        .join(r"System32\AMD\ANR\AMDNoiseSuppression.exe")
}
pub fn supported() -> Result<()> {
    let bytes = std::fs::read(executable()).context("未安装 AMD Noise Suppression")?;
    let digest = format!("{:x}", Sha256::digest(bytes));
    if digest != PROFILE_HASH {
        bail!("此 AMD 后台版本尚未验证，已停止私有协议写入。SHA-256: {digest}");
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
                EVENT_MODIFY_STATE | SYNCHRONIZE,
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
    fn request(&self, device: &str, enabled: Option<bool>) -> Result<bool> {
        let id = wide(device);
        if id.len() > 128 || device.is_empty() || device.contains('\0') {
            bail!("AMD 设备 ID 超出协议范围");
        }
        let (command, offset, result) = if enabled.is_some() {
            (0x87u32, 0x978, 0xa7c)
        } else {
            (0x86, 0x868, 0x96c)
        };
        unsafe {
            let mutex = handle(CreateMutexW(
                ptr::null(),
                0,
                wide(r"Local\AudioPin.AmdIpc.v1").as_ptr(),
            ))?;
            let acquired = WaitForSingleObject(mutex.0, 6000);
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
            let p = self.view.Value.cast::<u8>();
            // Only touch the verified command's fields. Never create or resize AMD's map.
            ptr::write_bytes(p.add(offset), 0, 256);
            ptr::copy_nonoverlapping(id.as_ptr().cast::<u8>(), p.add(offset), id.len() * 2);
            if let Some(state) = enabled {
                ptr::write_volatile(p.add(offset + 256), u8::from(state));
            }
            ptr::write_volatile(p.add(result).cast::<i32>(), i32::MIN);
            ptr::write_volatile(p.add(offset - 8).cast::<u64>(), 0);
            ptr::write_volatile(p.add(0x818).cast::<u32>(), command);
            if ResetEvent(self.done.0) == 0 || SetEvent(self.start.0) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // AMD's completion event is shared with Radeon Software and auto-reset.
            // Another client may consume it, or replace the global command after
            // our response. The command-specific completion marker + result are
            // the authority; short bounded waits also notice a consumed event.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let stamped = ptr::read_volatile(p.add(offset - 8).cast::<u64>()) != 0;
                let finished = ptr::read_volatile(p.add(result).cast::<i32>()) != i32::MIN;
                if stamped && finished {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    if ptr::read_volatile(p.add(0x818).cast::<u32>()) != command {
                        bail!("AMD 控制请求被其他客户端占用，将稍后重试");
                    }
                    bail!("AMD 后台响应超时，将退避后重试");
                }
                WaitForSingleObject(self.done.0, 50);
            }
            let returned = std::slice::from_raw_parts(p.add(offset).cast::<u16>(), id.len());
            if returned != id {
                bail!("AMD 响应设备不匹配，将稍后重试");
            }
            let rc = ptr::read_volatile(p.add(result).cast::<i32>());
            if rc != 0 {
                bail!("AMD 返回错误 {rc}，设备可能已断开");
            }
            Ok(ptr::read_volatile(p.add(offset + 256)) == 1)
        }
    }
    pub fn state(&self, device: &str) -> Result<bool> {
        self.request(device, None)
    }
    pub fn enable(&self, device: &str) -> Result<()> {
        self.request(device, Some(true))?;
        if !self.state(device)? {
            bail!("AMD 接收了开启命令，但运行状态仍为关闭");
        }
        Ok(())
    }
}
