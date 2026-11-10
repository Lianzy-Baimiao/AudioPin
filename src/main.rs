#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod amd;
mod audio;
mod config;
mod engine;
mod guard;
mod platform;
mod ui;
mod updates;

use anyhow::Context;

fn main() {
    if let Err(e) = entry() {
        let text = format!("{e:#}");
        eprintln!("{text}");
        if !std::env::args()
            .any(|a| a == "--diagnose" || a == "--check-update" || a == "--clear-app-routing")
        {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    std::ptr::null_mut(),
                    platform::wide(text).as_ptr(),
                    platform::wide("AudioPin").as_ptr(),
                    0x10,
                );
            }
        }
        std::process::exit(1);
    }
}
fn entry() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let mut c = config::Config::load()?;
    if let Some(at) = args.iter().position(|a| a == "--self-test-routing") {
        let _com = audio::Com::init()?;
        let audio = audio::Audio::new()?;
        let policy = audio::AppPolicy::new()?;
        // Optional target PID; defaults to this process.
        let pid = match args.get(at + 1) {
            Some(p) => p.parse().context("PID 无效")?,
            None => std::process::id(),
        };
        let id = audio::AppPolicy::full_id(&audio.default(1)?);
        for role in [0, 1, 2] {
            let original = policy.get(pid, role).context("读取偏好失败")?;
            let test = policy.set_raw(pid, role, &id).context("写入偏好失败");
            let restore = policy.set_raw(pid, role, &original).context("还原偏好失败");
            test?;
            restore?;
            println!("PASS: PID {pid}, capture role {role}: set / readback / restore");
            let default = audio.default(role)?;
            audio.set_default(&default, role)?;
            println!("PASS: default capture role {role}: idempotent set / readback");
        }
        return Ok(());
    }
    // Windows keeps app input preferences after AudioPin exits and only
    // accepts a live PID. Reset one app to "default" for every capture role.
    if let Some(at) = args.iter().position(|a| a == "--clear-app-routing") {
        let pid: u32 = args
            .get(at + 1)
            .context("用法：--clear-app-routing <PID>")?
            .parse()
            .context("PID 无效")?;
        let _com = audio::Com::init()?;
        let policy = audio::AppPolicy::new()?;
        for role in [0, 1, 2] {
            let before = policy.get(pid, role)?;
            policy.set_raw(pid, role, "")?;
            println!("PID {pid}, capture role {role}: [{before}] -> default");
        }
        return Ok(());
    }
    if args.iter().any(|a| a == "--diagnose") {
        let _com = audio::Com::init()?;
        let a = audio::Audio::new()?;
        let devices = a.devices()?;
        let supported = amd::supported();
        let configured_id = amd::configured_input();
        // Diagnose the pinned device, even when AMD's configuration drifted.
        let id = if c.physical_input.is_empty() {
            configured_id.clone()
        } else {
            c.physical_input.clone()
        };
        let state = if supported.is_ok() && !id.is_empty() {
            amd::Amd::connect()
                .and_then(|ipc| ipc.state(&id))
                .map(|b| b.to_string())
                .unwrap_or_else(|e| format!("{e:#}"))
        } else {
            "未查询".into()
        };
        let sessions = a.capture_sessions(&id).map_err(|e| format!("{e:#}"));
        let amd_pids: Vec<u32> = platform::amd_processes()?
            .into_iter()
            .map(|p| p.pid)
            .collect();
        let capture_active = sessions.as_ref().ok().map(|sessions| {
            sessions
                .iter()
                .any(|s| s.active && amd_pids.contains(&s.process_id))
        });
        let result = serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"amd_profile":supported.map(|_|"verified".into()).unwrap_or_else(|e|format!("{e:#}")),"amd_configured_input":configured_id,"pinned_physical_input":id,"amd_runtime_enabled":state,"physical_capture_sessions":sessions,"amd_process_ids":amd_pids,"amd_capture_active":capture_active,"devices":devices,"default_input":a.default(1).ok(),"communications_input":a.default(2).ok(),"app_policy_supported":audio::AppPolicy::new().is_ok(),"autostart":platform::is_autostart(),"guard_enabled":c.guard_enabled,"amd_enabled":c.amd_enabled});
        let text = serde_json::to_string_pretty(&result)?;
        std::fs::create_dir_all(config::data_dir())?;
        std::fs::write(config::data_dir().join("diagnostics.json"), &text)?;
        println!("{text}");
        return Ok(());
    }
    if args.iter().any(|a| a == "--check-update") {
        std::fs::create_dir_all(config::data_dir())?;
        let result = updates::check(&c);
        let text = match &result {
            Ok(s) => s.clone(),
            Err(e) => format!("检查失败：{e:#}"),
        };
        std::fs::write(config::data_dir().join("update-check.txt"), &text)?;
        println!("{text}");
        result?;
        return Ok(());
    }
    if c.physical_input.is_empty() {
        c.physical_input = amd::configured_input();
    }
    c.autostart = platform::is_autostart();
    ui::run(c, args.iter().any(|a| a == "--background"))
}
