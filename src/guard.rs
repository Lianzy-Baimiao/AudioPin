use crate::{
    amd,
    audio::{AppPolicy, Audio, Com, Device},
    config::{data_dir, Config},
    platform,
};
use anyhow::{bail, Context, Result};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct Snapshot {
    pub title: String,
    pub detail: String,
    pub devices: Vec<Device>,
    pub events: Vec<String>,
    pub pending_notification: Option<String>,
    pub healthy: bool,
    pub repaired: u64,
    pub checks: u64,
    pub update: String,
    pub apps: String,
}
pub enum Command {
    Apply(Config),
    Check,
    Pause(bool),
    Stop,
}
pub type Shared = Arc<Mutex<Snapshot>>;
pub fn start(config: Config) -> (Sender<Command>, Shared, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Snapshot {
        title: "正在检查".into(),
        update: "尚未检查 GitHub 更新".into(),
        ..Default::default()
    }));
    let state = shared.clone();
    let thread = std::thread::spawn(move || {
        if let Err(e) = run(config, rx, state.clone()) {
            let mut s = state.lock().unwrap();
            s.title = "守护未启动".into();
            s.detail = format!("{e:#}");
            log(&mut s, &format!("守护线程错误：{e:#}"));
        }
    });
    (tx, shared, thread)
}

/// Retry state is independent from UI and notifications. Missing devices never trigger writes.
#[derive(Default)]
pub struct Backoff {
    failures: u32,
    next: Option<Instant>,
}
impl Backoff {
    fn ready(&self, now: Instant) -> bool {
        self.next.is_none_or(|n| now >= n)
    }
    fn fail(&mut self, now: Instant, max: u64) {
        self.failures = self.failures.saturating_add(1);
        self.next =
            Some(now + Duration::from_secs((2u64.saturating_pow(self.failures.min(10))).min(max)));
    }
    fn reset(&mut self) {
        self.failures = 0;
        self.next = None;
    }
}
fn run(mut config: Config, rx: Receiver<Command>, shared: Shared) -> Result<()> {
    let _com = Com::init()?;
    let audio = Audio::new()?;
    let mut profile_error = amd::supported().err().map(|e| e.to_string());
    let mut profile_stamp = fs::metadata(amd::executable())
        .ok()
        .map(|m| (m.len(), m.modified().ok()));
    let mut retry = Backoff::default();
    let mut previous = String::new();
    let mut last_notification: Option<Instant> = None;
    let mut last_devices: Vec<(String, bool)> = vec![];
    let mut startup_attempt = None;
    let mut pending_recovery = false;
    let mut route_originals: HashMap<(u32, String, i32), (String, String)> = HashMap::new();
    let mut default_originals: HashMap<i32, (String, String)> = HashMap::new();
    let mut first = true;
    loop {
        if !first {
            match rx.recv_timeout(Duration::from_secs(config.poll_seconds)) {
                Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(Command::Apply(c)) => {
                    if c.physical_input != config.physical_input {
                        pending_recovery = false;
                    }
                    config = c;
                    retry.reset();
                }
                Ok(Command::Pause(pause)) => {
                    config.guard_enabled = !pause;
                    retry.reset();
                }
                Ok(Command::Check) => retry.reset(),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        first = false;
        let now = Instant::now();
        let stamp = fs::metadata(amd::executable())
            .ok()
            .map(|m| (m.len(), m.modified().ok()));
        if stamp != profile_stamp {
            profile_error = amd::supported().err().map(|e| e.to_string());
            profile_stamp = stamp;
            retry.reset();
        }
        let mut changes = vec![];
        let devices = match audio.devices() {
            Ok(d) => d,
            Err(e) => {
                publish(
                    &shared,
                    &config,
                    "音频服务不可用",
                    &format!("{e:#}"),
                    false,
                    &mut previous,
                    &mut last_notification,
                    vec![],
                );
                continue;
            }
        };
        let device_states: Vec<_> = devices.iter().map(|d| (d.id.clone(), d.active)).collect();
        if device_states != last_devices {
            retry.reset();
            last_devices = device_states;
            changes.push("音频设备列表/连接状态已更新".into());
        }
        shared.lock().unwrap().devices = devices.clone();
        let process_list = platform::processes().unwrap_or_default();
        if !config.pin_apps {
            shared.lock().unwrap().apps = "应用输入守护未启用".into();
        }
        // Restore only settings this process actually changed, and only if still ours.
        if !route_originals.is_empty() {
            if let Ok(policy) = AppPolicy::new() {
                route_originals.retain(|(pid, name, role), (original, applied)| {
                    if !process_list.iter().any(|(p, n)| p == pid && n == name) {
                        return false;
                    }
                    if config.guard_enabled
                        && config.pin_apps
                        && config.apps.iter().any(|a| a.eq_ignore_ascii_case(name))
                    {
                        return true;
                    }
                    match policy.get(*pid, *role) {
                        Ok(current) if current == *applied => {
                            match policy.set_raw(*pid, *role, original) {
                                Ok(()) => false,
                                Err(e) => {
                                    changes.push(format!("恢复应用路由失败：{e}"));
                                    true
                                }
                            }
                        }
                        Ok(_) => false,
                        Err(_) => true,
                    }
                });
            }
        }
        for role in [0, 1, 2] {
            let pin = config.guard_enabled
                && if role == 2 {
                    config.pin_communications
                } else {
                    config.pin_default
                };
            if !pin {
                if let Some((original, applied)) = default_originals.remove(&role) {
                    if audio.default(role).ok().as_deref() == Some(applied.as_str()) {
                        if let Err(e) = audio.set_default(&original, role) {
                            changes.push(format!("恢复默认设备失败：{e}"));
                            default_originals.insert(role, (original, applied));
                        }
                    }
                }
            }
        }
        if !config.guard_enabled {
            publish(
                &shared,
                &config,
                "守护已暂停",
                "不会自动开启降噪或更改音频输入。点击“恢复守护”继续。",
                false,
                &mut previous,
                &mut last_notification,
                changes,
            );
            continue;
        }
        if !retry.ready(now) {
            continue;
        }
        let outcome = (|| -> Result<(String, String)> {
            if config.amd_enabled {
                if let Some(error) = &profile_error {
                    bail!("{error}");
                }
                if config.physical_input.is_empty() {
                    bail!("请在“设备与路由”选择 AMD 使用的原始麦克风并保存");
                }
                let physical = devices
                    .iter()
                    .find(|d| d.id == config.physical_input && d.active)
                    .context("原始麦克风已断开，等待原设备重连；不会切换到远程虚拟麦克风")?;
                if physical.amd {
                    bail!("原始输入不能选择 AMD 降噪虚拟设备");
                }
                if !process_list
                    .iter()
                    .any(|(_, n)| n.eq_ignore_ascii_case("AMDNoiseSuppression.exe"))
                {
                    if !config.restart_amd_if_missing {
                        bail!("AMD 后台未运行；自动启动后台选项已关闭");
                    }
                    if startup_attempt
                        .is_none_or(|t: Instant| t.elapsed() >= Duration::from_secs(30))
                    {
                        platform::hidden_command(amd::executable())
                            .spawn()
                            .context("启动 AMD 后台失败")?;
                        startup_attempt = Some(now);
                        changes.push("AMD 后台缺失，已请求启动".into());
                    }
                    bail!("等待 AMD 后台启动");
                }
                let ipc = amd::Amd::connect()?;
                let enabled = ipc.state(&config.physical_input)?;
                let configured = amd::configured_on()?
                    && amd::input_is_pinned()?
                    && amd::configured_input() == config.physical_input;
                if !enabled || !configured {
                    pending_recovery = true;
                    changes.push(format!("检测到 AMD 降噪/输入偏离（运行开关：{enabled}）"));
                    amd::configure(&config.physical_input)?;
                    ipc.enable(&config.physical_input)?;
                    if !amd::configured_on()? || !ipc.state(&config.physical_input)? {
                        bail!("恢复后复查失败");
                    }
                }
                if pending_recovery {
                    // A previous command may have succeeded while its reply collided with
                    // Radeon Software. Only announce recovery after a fresh successful read.
                    changes.push("AMD 麦克风降噪已自动恢复，并通过后台状态复查".into());
                    shared.lock().unwrap().repaired += 1;
                    pending_recovery = false;
                }
            }
            let routing = config.pin_default || config.pin_communications || config.pin_apps;
            if routing {
                if config.output_input.is_empty() {
                    bail!("请选择提供给语音软件的输入设备");
                }
                if !devices
                    .iter()
                    .any(|d| d.id == config.output_input && d.active)
                {
                    bail!("目标输入设备离线，等待重连后恢复路由");
                }
                for role in [0, 1, 2] {
                    if (role == 2 && !config.pin_communications)
                        || (role != 2 && !config.pin_default)
                    {
                        continue;
                    }
                    let old = audio.default(role)?;
                    if old != config.output_input {
                        let record = default_originals
                            .entry(role)
                            .or_insert((old, config.output_input.clone()));
                        audio.set_default(&config.output_input, role)?;
                        record.1 = config.output_input.clone();
                        changes.push(format!(
                            "已恢复 Windows {}输入",
                            if role == 2 { "通信" } else { "默认" }
                        ));
                    }
                }
                if config.pin_apps {
                    let policy = AppPolicy::new()?;
                    let target = AppPolicy::full_id(&config.output_input);
                    let mut active = vec![];
                    for (pid, name) in &process_list {
                        if !config.apps.iter().any(|a| a.eq_ignore_ascii_case(name)) {
                            continue;
                        }
                        for role in [0, 1, 2] {
                            let old = policy.get(*pid, role)?;
                            if old != target {
                                let record = route_originals
                                    .entry((*pid, name.clone(), role))
                                    .or_insert((old, target.clone()));
                                policy.set_raw(*pid, role, &target)?;
                                record.1 = target.clone();
                                changes.push(format!(
                                    "已固定 {name} 的输入路由（PID {pid}，角色 {role}）"
                                ));
                            }
                        }
                        active.push(format!("{name} · PID {pid} · 系统路由已固定"));
                    }
                    shared.lock().unwrap().apps = if active.is_empty() {
                        "等待配置中的语音软件启动".into()
                    } else {
                        active.join("\r\n")
                    };
                }
            }
            retry.reset();
            Ok((
                "正在守护".into(),
                if config.amd_enabled {
                    "AMD 麦克风降噪运行开关已开启。设备断开、开关关闭或输入变更时自动恢复。\r\n此状态验证后台开关与设备连接，不评估音频内容或降噪效果。".into()
                } else {
                    "AMD 守护已关闭。按配置固定 Windows / 应用输入路由。".into()
                },
            ))
        })();
        shared.lock().unwrap().checks += 1;
        match outcome {
            Ok((title, detail)) => {
                let healthy = title == "正在守护";
                publish(
                    &shared,
                    &config,
                    &title,
                    &detail,
                    healthy,
                    &mut previous,
                    &mut last_notification,
                    changes,
                );
            }
            Err(e) => {
                retry.fail(now, config.retry_max_seconds);
                publish(
                    &shared,
                    &config,
                    "需要关注",
                    &format!("{e:#}"),
                    false,
                    &mut previous,
                    &mut last_notification,
                    changes,
                );
            }
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn publish(
    shared: &Shared,
    c: &Config,
    title: &str,
    detail: &str,
    healthy: bool,
    previous: &mut String,
    last: &mut Option<Instant>,
    changes: Vec<String>,
) {
    let mut s = shared.lock().unwrap();
    s.title = title.into();
    s.detail = detail.into();
    s.healthy = healthy;
    let key = format!("{title}|{detail}");
    let changed = *previous != key;
    if changed {
        log(&mut s, &format!("{title}：{detail}"));
        *previous = key;
    }
    for msg in &changes {
        log(&mut s, msg);
    }
    if c.notifications
        && (changed
            || (!changes.is_empty()
                && last.is_none_or(|t| {
                    t.elapsed() >= Duration::from_secs(c.notification_cooldown_seconds)
                })))
    {
        s.pending_notification = Some(if changes.iter().any(|c| c.contains("自动恢复")) {
            "AMD 麦克风降噪已自动恢复".into()
        } else {
            format!("{title}：{detail}")
        });
        *last = Some(Instant::now());
    }
}
pub fn log(s: &mut Snapshot, text: &str) {
    let mut time = windows_sys::Win32::Foundation::SYSTEMTIME::default();
    unsafe {
        windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut time);
    }
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}  {}",
        time.wYear,
        time.wMonth,
        time.wDay,
        time.wHour,
        time.wMinute,
        time.wSecond,
        text.replace(['\r', '\n'], " ")
    );
    if s.events.len() >= 200 {
        s.events.remove(0);
    }
    s.events.push(line.clone());
    let _ = fs::create_dir_all(data_dir());
    let path = data_dir().join("events.log");
    if fs::metadata(&path)
        .map(|m| m.len() > 1024 * 1024)
        .unwrap_or(false)
    {
        let _ = fs::rename(&path, data_dir().join("events.previous.log"));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_is_bounded_and_resets_after_reconnect() {
        let now = Instant::now();
        let mut b = Backoff::default();
        assert!(b.ready(now));
        b.fail(now, 60);
        assert!(!b.ready(now + Duration::from_secs(1)));
        assert!(b.ready(now + Duration::from_secs(2)));
        for _ in 0..40 {
            b.fail(now, 60);
        }
        assert!(!b.ready(now + Duration::from_secs(59)));
        assert!(b.ready(now + Duration::from_secs(60)));
        b.reset();
        assert!(b.ready(now));
    }
}
