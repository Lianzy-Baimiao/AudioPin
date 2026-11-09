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
pub fn amd_processes() -> Result<Vec<crate::engine::Process>> {
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
            result.push(crate::engine::Process {
                pid,
                name,
                started: started(&process)?,
            });
        }
    }
    Ok(result)
}
fn radeon_executable() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var_os("ProgramW6432")
            .or_else(|| std::env::var_os("ProgramFiles"))
            .unwrap_or_else(|| r"C:\Program Files".into()),
    )
    .join(r"AMD\CNext\CNext\RadeonSoftware.exe")
}
fn radeon_processes() -> Result<Vec<crate::engine::Process>> {
    let expected = radeon_executable();
    let mut result = Vec::new();
    for candidate in tracked_processes(&["RadeonSoftware.exe".into()])? {
        let process = process_handle(candidate.pid)?;
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
        if started(&process)? != candidate.started
            || !String::from_utf16_lossy(&path[..len as usize])
                .eq_ignore_ascii_case(&expected.to_string_lossy())
        {
            bail!("Radeon 面板进程身份不匹配，未重载");
        }
        result.push(candidate);
    }
    Ok(result)
}
pub struct PanelReload {
    child: std::process::Child,
    before: Vec<crate::engine::Process>,
    sent: std::time::Instant,
    receipt_before: crate::amd::InputReceipt,
    input: String,
    confirmed: Option<std::time::Instant>,
    activated: bool,
}
impl PanelReload {
    pub fn matches_input(&self, input: &str) -> bool {
        self.input == input
    }
    pub fn poll(&mut self) -> Result<bool> {
        use std::time::Duration;
        let elapsed = self.sent.elapsed();
        if let Some(status) = self.child.try_wait()? {
            if !status.success() {
                bail!("AMD 面板重启命令失败：{status}");
            }
            let after = radeon_processes()?;
            if !after.is_empty() && after.iter().all(|p| !self.before.contains(p)) {
                // `cncmd restart` launches `atlogon`, which can leave ANR lazy
                // initialization pending indefinitely. A normal single-instance
                // activation is needed as well; request it hidden once, then
                // require the new initialization receipts below.
                if !self.activated {
                    unsafe {
                        let exe = radeon_executable();
                        let result = windows_sys::Win32::UI::Shell::ShellExecuteW(
                            std::ptr::null_mut(),
                            wide("open").as_ptr(),
                            wide(&exe).as_ptr(),
                            std::ptr::null(),
                            wide(exe.parent().unwrap()).as_ptr(),
                            windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE,
                        );
                        if result as usize <= 32 {
                            bail!(
                                "AMD 面板初始化启动失败，ShellExecute 返回 {}",
                                result as usize
                            );
                        }
                    }
                    self.activated = true;
                    return Ok(false);
                }
                let initialized = crate::amd::Amd::connect()
                    .map(|a| a.input_receipt().confirms_reload(&self.receipt_before))
                    .unwrap_or(false);
                if initialized {
                    let confirmed = self.confirmed.get_or_insert_with(std::time::Instant::now);
                    if confirmed.elapsed() >= Duration::from_secs(3) {
                        return Ok(true);
                    }
                } else {
                    self.confirmed = None;
                }
            }
        }
        if elapsed > Duration::from_secs(45) {
            let _ = self.child.kill();
            bail!("AMD 面板未在 45 秒内完成降噪输入初始化，将重试重载");
        }
        Ok(false)
    }
}
/// Use AMD's own restart command. It coordinates with cncmd's watch process;
/// killing Radeon and launching another copy ourselves creates competing restarts.
pub fn reload_amd_panel(input: &str, current: impl Fn() -> bool) -> Result<Option<PanelReload>> {
    use sha2::{Digest, Sha256};
    let before = radeon_processes()?;
    if before.is_empty() {
        return Ok(None);
    }
    let exe = radeon_executable();
    let command = exe.with_file_name("cncmd.exe");
    let library = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()),
    )
    .join(r"System32\ANR-bgproc-Lib.dll");
    for (path, expected) in [
        (
            &exe,
            "1d33caee9336ace73c6c15a56b5653d2516dcbcd94347dee9cd5d518f80a7243",
        ),
        (
            &command,
            "27239a28e2f914b6d0a230abaa7631dc7f77a39d6d0e36eac2474cf0a4247064",
        ),
        (
            &library,
            "d07610c76593e2cfb4d73644d879b87f5c2fdc6f7f0a0223c37d093b122b21d8",
        ),
    ] {
        let digest = format!("{:x}", Sha256::digest(std::fs::read(path)?));
        if digest != expected {
            bail!("此 Radeon 面板组件版本尚未验证，未重载：{}", path.display());
        }
    }
    if !current() {
        bail!("守护设置已改变，已取消面板重载");
    }
    let receipt_before = crate::amd::Amd::connect()?.input_receipt();
    let child = hidden_command(&command)
        .arg("restart")
        .current_dir(command.parent().unwrap())
        .spawn()?;
    Ok(Some(PanelReload {
        child,
        before,
        sent: std::time::Instant::now(),
        receipt_before,
        input: input.into(),
        confirmed: None,
        activated: false,
    }))
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
