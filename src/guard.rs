use crate::{
    amd,
    audio::{AppPolicy, Audio, Com, Device},
    config::{data_dir, Config},
    engine::{AmdObservation, AmdPassive, Backend, Engine, Process, State},
    platform,
};
use anyhow::{bail, Context, Result};
use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct Snapshot {
    pub title: String,
    pub detail: String,
    pub amd_detail: String,
    pub route_detail: String,
    pub devices: Vec<Device>,
    pub events: Vec<String>,
    pub notifications: VecDeque<String>,
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
pub struct Control {
    tx: mpsc::Sender<(u64, Command)>,
    generation: Arc<AtomicU64>,
}
impl Control {
    pub fn send(&self, command: Command) -> Result<()> {
        let generation = if matches!(command, Command::Check) {
            self.generation.load(Ordering::SeqCst)
        } else {
            self.generation.fetch_add(1, Ordering::SeqCst) + 1
        };
        self.tx
            .send((generation, command))
            .context("守护线程已停止，请重启 AudioPin")
    }
}
pub fn start(config: Config) -> (Control, Shared, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let generation = Arc::new(AtomicU64::new(0));
    let shared = Arc::new(Mutex::new(Snapshot {
        title: "正在检查".into(),
        update: "尚未检查更新".into(),
        ..Default::default()
    }));
    let state = shared.clone();
    let revision = generation.clone();
    let thread = std::thread::spawn(move || {
        if let Err(e) = run(config, rx, revision, state.clone()) {
            let mut s = state.lock().unwrap();
            s.title = "守护已停止".into();
            s.detail = format!("{e:#}");
            s.healthy = false;
            s.notifications.push_back(format!("守护已停止：{e:#}"));
            log(&mut s, &format!("守护线程错误：{e:#}"));
        }
    });
    (Control { tx, generation }, shared, thread)
}

struct WindowsBackend<'a> {
    audio: &'a Audio,
    processes: &'a [Process],
    generation: &'a AtomicU64,
    revision: u64,
    profile_error: Option<&'a str>,
    panel_reload: &'a mut Option<platform::PanelReload>,
}
impl WindowsBackend<'_> {
    fn check_current(&self) -> Result<()> {
        if !self.current() {
            bail!("新的设置正在应用，已取消旧设置写入");
        }
        Ok(())
    }
    fn check_profile(&self) -> Result<()> {
        if let Some(e) = self.profile_error {
            bail!("{e}");
        }
        Ok(())
    }
}
impl Backend for WindowsBackend<'_> {
    fn current(&self) -> bool {
        self.generation.load(Ordering::SeqCst) == self.revision
    }
    fn amd_passive(&mut self, id: &str) -> Result<AmdPassive> {
        self.check_profile()?;
        let backends = platform::amd_processes()?;
        if backends.is_empty() {
            return Ok(AmdPassive {
                backends,
                configured: false,
                capturing: false,
            });
        }
        let configured =
            amd::configured_on()? && amd::input_is_pinned()? && amd::configured_input() == id;
        let capturing = self
            .audio
            .capture_sessions(id)?
            .iter()
            .any(|s| s.active && backends.iter().any(|p| p.pid == s.process_id));
        Ok(AmdPassive {
            backends,
            configured,
            capturing,
        })
    }
    fn amd_query(&mut self, id: &str) -> Result<Option<bool>> {
        self.check_profile()?;
        match amd::Amd::connect()?.state(id) {
            Ok(enabled) => Ok(Some(enabled)),
            Err(e) if amd::uninitialized(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn amd_repair(&mut self, id: &str, observed: &AmdObservation) -> Result<()> {
        self.check_current()?;
        // Recheck the binary immediately before a private-protocol mutation.
        amd::supported()?;
        self.check_current()?;
        amd::configure(id)?;
        self.check_current()?;
        match observed {
            AmdObservation::Missing => {
                // A concurrently started verified backend should be observed on
                // the next cycle, not launched a second time.
                if platform::amd_processes()?.is_empty() {
                    platform::hidden_command(amd::executable())
                        .arg("NoQuit")
                        .current_dir(amd::executable().parent().context("AMD 安装目录无效")?)
                        .spawn()
                        .context("启动 AMD 后台失败")?;
                }
                Ok(())
            }
            AmdObservation::Initializing => amd::Amd::connect()?.initialize(|| self.current()),
            AmdObservation::Running { .. } => amd::Amd::connect()?.enable(id, || self.current()),
        }
    }
    fn amd_sync_panel(&mut self, input: &str) -> Result<bool> {
        self.check_current()?;
        if let Some(reload) = self.panel_reload.as_mut() {
            let result = reload.poll();
            let same_input = reload.matches_input(input);
            if !matches!(result, Ok(false)) {
                *self.panel_reload = None;
            }
            if !same_input && matches!(result, Ok(true)) {
                // A configuration change must not inherit an old input's
                // initialization confirmation. Start a fresh reload next cycle.
                return Ok(false);
            }
            return result;
        }
        *self.panel_reload = platform::reload_amd_panel(input, || self.current())?;
        Ok(self.panel_reload.is_none())
    }

    fn default(&mut self, role: i32) -> Result<String> {
        self.audio.default(role)
    }
    fn set_default(&mut self, role: i32, id: &str) -> Result<()> {
        self.check_current()?;
        self.audio.set_default(id, role)
    }
    fn app_get(&mut self, process: &Process, role: i32) -> Result<String> {
        platform::verify_process(process)?;
        // Windows rejects console processes (e.g. ping.exe) with
        // E_INVALIDARG here; GUI processes such as explorer.exe work.
        AppPolicy::new()?
            .get(process.pid, role)
            .context("Windows 不接受此进程的输入偏好，控制台程序或尚未使用音频的进程常见此情况")
    }
    fn app_set(&mut self, process: &Process, role: i32, id: &str) -> Result<()> {
        self.check_current()?;
        if !self.processes.contains(process) {
            bail!("目标进程已改变");
        }
        platform::verify_process(process)?;
        AppPolicy::new()?.set_raw(process.pid, role, id)
    }
}
fn run(
    mut config: Config,
    rx: Receiver<(u64, Command)>,
    generation: Arc<AtomicU64>,
    shared: Shared,
) -> Result<()> {
    let _com = Com::init()?;
    // Windows Audio can still be starting at login. Keep the worker alive and
    // recreate the enumerator on the next cycle instead of exiting permanently.
    let mut audio = None;
    let mut engine = Engine::default();
    engine.apply(&config);
    let mut revision = 0;
    let mut first = true;
    let mut profile_error = amd::supported().err().map(|e| format!("{e:#}"));
    let mut stamp = profile_stamp();
    let mut previous = String::new();
    let mut last_event_notification = None;
    let mut last_processes = Vec::new();
    let mut panel_reload = None;
    loop {
        let received = if first {
            first = false;
            None
        } else {
            match rx.recv_timeout(Duration::from_secs(config.poll_seconds)) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        };
        // Drain queued configuration changes before doing another observation.
        // Generation checks also cancel writes in an already-running cycle.
        for (next, command) in received.into_iter().chain(rx.try_iter()) {
            match command {
                Command::Stop => return Ok(()),
                Command::Apply(c) => {
                    config = c;
                    revision = next;
                    engine.apply(&config);
                }
                Command::Pause(pause) => {
                    config.guard_enabled = !pause;
                    revision = next;
                    engine.apply(&config);
                }
                Command::Check => engine.recheck(),
            }
        }
        let current_stamp = profile_stamp();
        if current_stamp != stamp {
            profile_error = amd::supported().err().map(|e| format!("{e:#}"));
            stamp = current_stamp;
            engine.recheck();
        }
        let devices_result = (|| -> Result<Vec<Device>> {
            if audio.is_none() {
                audio = Some(Audio::new()?);
            }
            audio.as_ref().context("音频服务尚未就绪")?.devices()
        })();
        let devices = match devices_result {
            Ok(d) => d,
            Err(e) => {
                audio = None;
                if generation.load(Ordering::SeqCst) != revision {
                    continue;
                }
                let mut s = shared.lock().unwrap();
                s.devices.clear();
                s.amd_detail = "音频服务不可用，采集状态未知".into();
                s.route_detail = "音频服务不可用，输入状态未知".into();
                s.checks += 1;
                publish(
                    &mut s,
                    &config,
                    "音频服务不可用",
                    &format!("{e:#}"),
                    false,
                    &mut previous,
                    &mut last_event_notification,
                    vec![],
                    false,
                );
                continue;
            }
        };
        let configured_apps = if config.pin_apps && config.guard_enabled {
            config.apps.as_slice()
        } else {
            &[]
        };
        let process_result = platform::tracked_processes(&engine.process_names(configured_apps));
        if let Ok(processes) = &process_result {
            last_processes = processes.clone();
        }
        let processes = &last_processes;
        let mut backend = WindowsBackend {
            audio: audio.as_ref().context("音频服务尚未就绪")?,
            processes,
            generation: &generation,
            revision,
            profile_error: profile_error.as_deref(),
            panel_reload: &mut panel_reload,
        };
        let before = engine.repaired;
        let mut cycle = engine.cycle(&config, &devices, processes, &mut backend, Instant::now());
        if config.guard_enabled && config.pin_apps {
            if let Err(e) = process_result {
                cycle.routes.state = State::Error;
                cycle.routes.detail = format!("无法检查应用进程：{e:#}");
            }
        }
        // Never publish a stale healthy result after a Pause/Apply was queued.
        if generation.load(Ordering::SeqCst) != revision {
            continue;
        }
        let all = [&cycle.amd.state, &cycle.routes.state];
        let enabled = all.iter().any(|s| **s != State::Disabled);
        let healthy = config.guard_enabled
            && enabled
            && all
                .iter()
                .all(|s| matches!(s, State::Ready | State::Disabled));
        let title = if !config.guard_enabled {
            "守护已暂停"
        } else if all.contains(&&State::Error) {
            "需要关注"
        } else if all.contains(&&State::Repairing) {
            "正在恢复"
        } else if all.contains(&&State::Waiting) {
            "等待设备或设置"
        } else if enabled {
            "守护运行中"
        } else {
            "尚未启用守护项目"
        };
        let detail = if !config.guard_enabled {
            if cycle.routes.state == State::Error {
                format!(
                    "已停止守护写入；部分旧路由还原失败：{}",
                    cycle.routes.detail
                )
            } else {
                "自动恢复已暂停。音频设备保持当前可用状态。".into()
            }
        } else if healthy {
            "后台状态与 Windows 输入已检查。通话输入和降噪效果需在语音软件中确认。".into()
        } else {
            format!("{}\r\n{}", cycle.amd.detail, cycle.routes.detail)
        };
        let mut s = shared.lock().unwrap();
        s.devices = devices;
        s.amd_detail = cycle.amd.detail;
        s.route_detail = cycle.routes.detail;
        s.apps = cycle.apps;
        s.checks += 1;
        s.repaired = engine.repaired;
        publish(
            &mut s,
            &config,
            title,
            &detail,
            healthy,
            &mut previous,
            &mut last_event_notification,
            cycle.events,
            engine.repaired > before,
        );
    }
    Ok(())
}
fn profile_stamp() -> Vec<Option<(u64, Option<std::time::SystemTime>)>> {
    [
        amd::executable(),
        amd::executable().with_file_name("anronnxlib.dll"),
    ]
    .into_iter()
    .map(|path| {
        fs::metadata(path)
            .ok()
            .map(|m| (m.len(), m.modified().ok()))
    })
    .collect()
}
#[allow(clippy::too_many_arguments)]
fn publish(
    s: &mut Snapshot,
    c: &Config,
    title: &str,
    detail: &str,
    healthy: bool,
    previous: &mut String,
    last: &mut Option<Instant>,
    events: Vec<String>,
    recovered: bool,
) {
    s.title = title.into();
    s.detail = detail.into();
    s.healthy = healthy;
    let key = format!("{title}|{detail}");
    let changed = *previous != key;
    if changed {
        log(s, &format!("{title}：{detail}"));
        *previous = key;
    }
    for msg in &events {
        log(s, msg);
    }
    if c.notifications
        && (changed
            || recovered
            || (!events.is_empty()
                && last.is_none_or(|t| {
                    t.elapsed() >= Duration::from_secs(c.notification_cooldown_seconds)
                })))
    {
        if s.notifications.len() >= 16 {
            s.notifications.pop_front();
        }
        s.notifications.push_back(if recovered {
            "AMD 后台恢复已确认；指定麦克风采集活动".into()
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
