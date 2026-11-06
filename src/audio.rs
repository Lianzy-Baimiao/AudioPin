use crate::platform::com_method;
use anyhow::{bail, Result};
use serde::Serialize;
use std::ffi::c_void;
use windows::{
    core::{IUnknown, Interface, GUID, HRESULT, HSTRING, PCWSTR},
    Win32::{
        Media::Audio::*,
        System::{Com::*, WinRT::RoGetActivationFactory},
    },
};
use winreg::{enums::*, RegKey};

pub struct Com;
impl Com {
    pub fn init() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub amd: bool,
}
pub struct Audio {
    enumerator: IMMDeviceEnumerator,
}
#[derive(Debug, Serialize)]
pub struct CaptureSession {
    pub process_id: u32,
    pub active: bool,
}
impl Audio {
    pub fn new() -> Result<Self> {
        Ok(Self {
            enumerator: unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? },
        })
    }
    pub fn devices(&self) -> Result<Vec<Device>> {
        unsafe {
            let collection = self
                .enumerator
                .EnumAudioEndpoints(eCapture, DEVICE_STATE(DEVICE_STATEMASK_ALL))?;
            let mut result = vec![];
            for i in 0..collection.GetCount()? {
                let d = collection.Item(i)?;
                let p = d.GetId()?;
                let id = p.to_string()?;
                CoTaskMemFree(Some(p.0.cast()));
                let guid = id.rsplit('.').next().unwrap_or_default();
                let key = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\{guid}\Properties"));
                let name = key
                    .map(|k| {
                        let label: String = k
                            .get_value("{a45c254e-df1c-4efd-8020-67d146a850e0},2")
                            .unwrap_or_default();
                        let adapter: String = k
                            .get_value("{b3f8fa53-0004-438e-9003-51a46e139bfc},6")
                            .unwrap_or_default();
                        format!("{label} ({adapter})")
                    })
                    .unwrap_or_else(|_| id.clone());
                let amd = name.to_ascii_lowercase().contains("amd streaming")
                    || name.to_ascii_lowercase().contains("amd noise");
                result.push(Device {
                    id,
                    name,
                    active: d.GetState()? == DEVICE_STATE_ACTIVE,
                    amd,
                });
            }
            result.sort_by_key(|d| (!d.active, d.name.clone()));
            Ok(result)
        }
    }
    pub fn default(&self, role: i32) -> Result<String> {
        unsafe {
            let device = self
                .enumerator
                .GetDefaultAudioEndpoint(eCapture, ERole(role))?;
            let p = device.GetId()?;
            let id = p.to_string();
            CoTaskMemFree(Some(p.0.cast()));
            Ok(id?)
        }
    }
    /// Inspect Windows' capture sessions without opening or recording the microphone.
    pub fn capture_sessions(&self, id: &str) -> Result<Vec<CaptureSession>> {
        unsafe {
            let id = crate::platform::wide(id);
            let device = self.enumerator.GetDevice(PCWSTR(id.as_ptr()))?;
            let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
            let sessions = manager.GetSessionEnumerator()?;
            let mut result = Vec::new();
            for index in 0..sessions.GetCount()? {
                let session: IAudioSessionControl2 = sessions.GetSession(index)?.cast()?;
                result.push(CaptureSession {
                    process_id: session.GetProcessId()?,
                    active: session.GetState()? == AudioSessionStateActive,
                });
            }
            Ok(result)
        }
    }
    pub fn set_default(&self, id: &str, role: i32) -> Result<()> {
        unsafe {
            let object: IUnknown = CoCreateInstance(
                &GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9),
                None,
                CLSCTX_ALL,
            )?;
            let mut raw = std::ptr::null_mut();
            object
                .query(
                    &GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8),
                    &mut raw,
                )
                .ok()?;
            let policy = IUnknown::from_raw(raw);
            let call: unsafe extern "system" fn(*mut c_void, PCWSTR, i32) -> HRESULT =
                com_method(policy.as_raw(), 13);
            let id16 = crate::platform::wide(id);
            call(policy.as_raw(), PCWSTR(id16.as_ptr()), role).ok()?;
        }
        if self.default(role)? != id {
            bail!("Windows 默认输入复查未通过");
        }
        Ok(())
    }
}
pub struct AppPolicy(IUnknown);
impl AppPolicy {
    pub fn new() -> Result<Self> {
        unsafe {
            let factory: IUnknown =
                RoGetActivationFactory(&HSTRING::from("Windows.Media.Internal.AudioPolicyConfig"))?;
            for iid in [
                0xab3d4648_e242_459f_b02f_541c70306324u128,
                0x2a59116d_6c4f_45e0_a74f_707e3fef9258,
            ] {
                let mut raw = std::ptr::null_mut();
                if factory.query(&GUID::from_u128(iid), &mut raw).is_ok() {
                    return Ok(Self(IUnknown::from_raw(raw)));
                }
            }
        }
        bail!("当前 Windows 不支持应用音频路由接口")
    }
    pub fn get(&self, pid: u32, role: i32) -> Result<String> {
        unsafe {
            let call: unsafe extern "system" fn(
                *mut c_void,
                u32,
                i32,
                i32,
                *mut HSTRING,
            ) -> HRESULT = com_method(self.0.as_raw(), 26);
            let mut value = HSTRING::new();
            call(self.0.as_raw(), pid, 1, role, &mut value).ok()?;
            Ok(value.to_string_lossy())
        }
    }
    pub fn set_raw(&self, pid: u32, role: i32, id: &str) -> Result<()> {
        unsafe {
            let call: unsafe extern "system" fn(
                *mut c_void,
                u32,
                i32,
                i32,
                *mut c_void,
            ) -> HRESULT = com_method(self.0.as_raw(), 25);
            let value = HSTRING::from(id);
            call(
                self.0.as_raw(),
                pid,
                1,
                role,
                std::mem::transmute_copy(&value),
            )
            .ok()?;
        }
        if self.get(pid, role)? != id {
            bail!("应用输入路由复查未通过（PID {pid}）");
        }
        Ok(())
    }
    pub fn full_id(id: &str) -> String {
        format!(r"\\?\SWD#MMDEVAPI#{id}#{{2eef81be-33fa-4800-9670-1cd474972c3f}}")
    }
}
