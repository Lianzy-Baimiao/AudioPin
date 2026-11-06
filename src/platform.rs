use anyhow::{bail, Result};
use std::{
    ffi::{c_void, OsStr},
    os::windows::{ffi::OsStrExt, process::CommandExt},
    process::Command,
};
use windows_sys::Win32::{Foundation::*, System::Diagnostics::ToolHelp::*};

pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}
pub struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                CloseHandle(self.0);
            }
        }
    }
}
pub fn handle(h: HANDLE) -> Result<Handle> {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(Handle(h))
    }
}
pub fn hidden_command(exe: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(exe);
    command.creation_flags(0x08000000);
    command
}
pub fn processes() -> Result<Vec<(u32, String)>> {
    unsafe {
        let snap = handle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0))?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..std::mem::zeroed()
        };
        let mut items = vec![];
        if Process32FirstW(snap.0, &mut entry) == 0 {
            bail!("无法枚举进程");
        }
        loop {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            items.push((
                entry.th32ProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..len]),
            ));
            if Process32NextW(snap.0, &mut entry) == 0 {
                break;
            }
        }
        Ok(items)
    }
}
fn process_handle(pid: u32) -> Result<Handle> {
    unsafe {
        handle(windows_sys::Win32::System::Threading::OpenProcess(
            windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        ))
    }
}
fn started(process: &Handle) -> Result<u64> {
    unsafe {
        let (mut created, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        if windows_sys::Win32::System::Threading::GetProcessTimes(
            process.0,
            &mut created,
            &mut exit,
            &mut kernel,
            &mut user,
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}
fn in_current_session(pid: u32) -> bool {
    unsafe {
        use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
        use windows_sys::Win32::System::Threading::*;
        let (mut own, mut other) = (0, 0);
        ProcessIdToSessionId(GetCurrentProcessId(), &mut own) != 0
            && ProcessIdToSessionId(pid, &mut other) != 0
            && own == other
    }
}
pub fn tracked_processes(names: &[String]) -> Result<Vec<crate::engine::Process>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for (pid, name) in processes()? {
        if !names.iter().any(|n| n.eq_ignore_ascii_case(&name)) || !in_current_session(pid) {
            continue;
        }
        let handle = process_handle(pid)?;
        result.push(crate::engine::Process {
            pid,
            name,
            started: started(&handle)?,
        });
    }
    Ok(result)
}
pub fn verify_process(process: &crate::engine::Process) -> Result<()> {
    if !in_current_session(process.pid)
        || started(&process_handle(process.pid)?)? != process.started
    {
        bail!("应用已退出或进程标识已变化");
    }
    Ok(())
}
/// Restrict AMD identity to the installed executable in this Windows session.
/// A filename-only match could accept another login session or a renamed EXE.
pub fn amd_process_ids() -> Result<Vec<u32>> {
    let mut result = Vec::new();
    for (pid, name) in processes()? {
        if !name.eq_ignore_ascii_case("AMDNoiseSuppression.exe") || !in_current_session(pid) {
            continue;
        }
        let process = process_handle(pid)?;
        let mut path = vec![0u16; 32768];
        let mut len = path.len() as u32;
        unsafe {
            if windows_sys::Win32::System::Threading::QueryFullProcessImageNameW(
                process.0,
                0,
                path.as_mut_ptr(),
                &mut len,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        if String::from_utf16_lossy(&path[..len as usize])
            .eq_ignore_ascii_case(&crate::amd::executable().to_string_lossy())
        {
            result.push(pid);
        }
    }
    Ok(result)
}
pub fn autostart(enable: bool) -> Result<()> {
    use winreg::{enums::*, RegKey};
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")?;
    if enable {
        let exe = std::env::current_exe()?;
        key.set_value("AudioPin", &format!("\"{}\" --background", exe.display()))?;
    } else if let Err(err) = key.delete_value("AudioPin") {
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(err.into());
        }
    }
    Ok(())
}
pub fn is_autostart() -> bool {
    use winreg::{enums::*, RegKey};
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
        .and_then(|k| k.get_value::<String, _>("AudioPin"))
        .is_ok()
}
pub fn open(target: &str) {
    unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            wide(target).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        );
    }
}
pub unsafe fn com_method<T: Copy>(raw: *mut c_void, slot: usize) -> T {
    let table = *(raw as *const *const *const c_void);
    std::mem::transmute_copy(&*table.add(slot))
}
