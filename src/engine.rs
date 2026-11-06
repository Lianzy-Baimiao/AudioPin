//! One observation cycle. Windows/AMD access is behind Backend so failures and
//! command races can be tested without changing the user's audio configuration.
use crate::{audio::Device, config::Config};
use anyhow::{bail, Result};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Disabled,
    Ready,
    Waiting,
    Repairing,
    Error,
}
#[derive(Clone, Debug)]
pub struct Health {
    pub state: State,
    pub detail: String,
}
impl Health {
    fn new(state: State, detail: impl Into<String>) -> Self {
        Self {
            state,
            detail: detail.into(),
        }
    }
    fn error(e: impl std::fmt::Display) -> Self {
        Self::new(State::Error, e.to_string())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Process {
    pub pid: u32,
    pub name: String,
    // Creation time prevents a reused PID from inheriting a restore record.
    pub started: u64,
}
#[derive(Clone, Debug)]
pub enum AmdObservation {
    Missing,
    Initializing,
    Running {
        enabled: bool,
        configured: bool,
        capturing: bool,
    },
}
pub trait Backend {
    fn current(&self) -> bool;
    fn amd_observe(&mut self, id: &str) -> Result<AmdObservation>;
    fn amd_repair(&mut self, id: &str, observed: &AmdObservation) -> Result<()>;
    fn default(&mut self, role: i32) -> Result<String>;
    fn set_default(&mut self, role: i32, id: &str) -> Result<()>;
    fn app_get(&mut self, process: &Process, role: i32) -> Result<String>;
    fn app_set(&mut self, process: &Process, role: i32, id: &str) -> Result<()>;
}
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
            Some(now + Duration::from_secs(2u64.saturating_pow(self.failures.min(10)).min(max)));
    }
    fn reset(&mut self) {
        *self = Self::default();
    }
}
#[derive(Default)]
struct Recovery {
    pending: bool,
    sent: Option<Instant>,
    retry: Backoff,
}
#[derive(Clone)]
struct Original {
    before: String,
    applied: Vec<String>,
}
#[derive(Default)]
pub struct Engine {
    amd: Recovery,
    defaults: HashMap<i32, Original>,
    apps: HashMap<(Process, i32), Original>,
    route_retry: HashMap<String, Backoff>,
    device_key: Vec<(String, bool)>,
    physical: String,
    pub repaired: u64,
}
pub struct Cycle {
    pub amd: Health,
    pub routes: Health,
    pub apps: String,
    pub events: Vec<String>,
}
impl Engine {
    pub fn process_names(&self, configured: &[String]) -> Vec<String> {
        let mut names = configured.to_vec();
        for (process, _) in self.apps.keys() {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(&process.name)) {
                names.push(process.name.clone());
            }
        }
        names
    }
    pub fn reset_retries(&mut self) {
        self.amd.retry.reset();
        self.route_retry.clear();
    }
    pub fn apply(&mut self, c: &Config) {
        if self.physical != c.physical_input || !c.amd_enabled || !c.guard_enabled {
            self.amd = Recovery::default();
        }
        self.physical = c.physical_input.clone();
        self.reset_retries();
    }
    pub fn cycle(
        &mut self,
        c: &Config,
        devices: &[Device],
        processes: &[Process],
        b: &mut impl Backend,
        now: Instant,
    ) -> Cycle {
        let mut key: Vec<_> = devices.iter().map(|d| (d.id.clone(), d.active)).collect();
        key.sort();
        let mut events = Vec::new();
        if key != self.device_key {
            // AMD's virtual endpoint also disconnects during engine startup.
            // That must not reset a failing backend's restart backoff.
            let physical_state = |list: &[(String, bool)]| {
                list.iter()
                    .find(|(id, _)| id == &c.physical_input)
                    .map(|(_, active)| *active)
            };
            if physical_state(&key) != physical_state(&self.device_key) {
                self.amd.retry.reset();
            }
            self.route_retry.clear();
            self.device_key = key;
            events.push("音频设备连接状态已更新".into());
        }
        // Routing must still run when AMD is unsupported, offline, or timing out.
        let (routes, apps) = self.routes(c, devices, processes, b, now, &mut events);
        let amd = self.check_amd(c, devices, b, now, &mut events);
        Cycle {
            amd,
            routes,
            apps,
            events,
        }
    }
    fn check_amd(
        &mut self,
        c: &Config,
        devices: &[Device],
        b: &mut impl Backend,
        now: Instant,
        events: &mut Vec<String>,
    ) -> Health {
        if !c.guard_enabled || !c.amd_enabled {
            return Health::new(State::Disabled, "AMD 守护未启用");
        }
        if c.physical_input.is_empty() {
            return Health::error("请先选择原始麦克风");
        }
        let Some(device) = devices
            .iter()
            .find(|d| d.id == c.physical_input && d.active)
        else {
            return Health::new(State::Waiting, "原始麦克风离线，等待原设备重连");
        };
        if device.amd {
            return Health::error("AMD 原始输入不能选择降噪虚拟设备");
        }
        if !b.current() {
            return Health::new(State::Waiting, "正在应用新的守护设置");
        }
        // Backoff throttles writes, never observations. A recovery is visible on
        // the next cycle even after a failed write with a long retry interval.
        let observation = match b.amd_observe(&c.physical_input) {
            Ok(o) => o,
            Err(e) => return Health::error(format!("AMD 检查失败：{e:#}")),
        };
        if matches!(
            observation,
            AmdObservation::Running {
                enabled: true,
                configured: true,
                capturing: true
            }
        ) {
            if self.amd.pending {
                self.repaired += 1;
                events
                    .push("AMD 后台恢复已确认：运行开关开启，指定麦克风的 AMD 采集会话活动".into());
            }
            self.amd = Recovery::default();
            return Health::new(State::Ready, "后台开关开启 · 指定麦克风采集活动");
        }
        if matches!(observation, AmdObservation::Missing) && !c.restart_amd_if_missing {
            return Health::error("AMD 后台未运行，自动启动已关闭");
        }
        if self
            .amd
            .sent
            .is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(8))
        {
            return Health::new(State::Repairing, "已请求恢复，正在等待 AMD 建立采集会话");
        }
        if !self.amd.retry.ready(now) {
            return Health::new(
                State::Error,
                "AMD 尚未恢复，等待下一次修复；继续检查运行状态",
            );
        }
        if !b.current() {
            return Health::new(State::Waiting, "正在应用新的守护设置");
        }
        // Capture the deadline after I/O: a slow/failed IPC must not consume its
        // own cooldown and cause an immediate retry storm.
        match b.amd_repair(&c.physical_input, &observation) {
            Ok(()) => {
                self.amd.pending = true;
                self.amd.sent = Some(now.max(Instant::now()));
                self.amd
                    .retry
                    .fail(now.max(Instant::now()), c.retry_max_seconds);
                events.push("检测到 AMD 偏离，已发送恢复请求，等待独立复查".into());
                Health::new(State::Repairing, "已请求恢复，正在等待 AMD 建立采集会话")
            }
            Err(e) => {
                self.amd
                    .retry
                    .fail(now.max(Instant::now()), c.retry_max_seconds);
                Health::error(format!("AMD 恢复失败：{e:#}"))
            }
        }
    }
    fn routes(
        &mut self,
        c: &Config,
        devices: &[Device],
        processes: &[Process],
        b: &mut impl Backend,
        now: Instant,
        events: &mut Vec<String>,
    ) -> (Health, String) {
        let mut errors = Vec::new();
        let target_online = devices.iter().any(|d| d.id == c.output_input && d.active);
        let routing = c.guard_enabled && (c.pin_default || c.pin_communications || c.pin_apps);
        for role in [0, 1, 2] {
            let pin = c.guard_enabled
                && if role == 2 {
                    c.pin_communications
                } else {
                    c.pin_default
                };
            let key = format!("default:{role}");
            let result = if pin && target_online {
                self.pin_default(
                    b,
                    role,
                    &c.output_input,
                    &key,
                    now,
                    c.retry_max_seconds,
                    events,
                )
            } else if !pin {
                self.restore_default(b, role, &key, now, c.retry_max_seconds, events)
            } else {
                Ok(())
            };
            if let Err(e) = result {
                errors.push(format!("Windows 输入角色 {role}：{e:#}"));
            }
        }
        let mut app_lines = Vec::new();
        // Only restore a live process with the same creation time. Windows app
        // preferences persist beyond a PID; dead processes cannot be restored
        // through this PID API and are never described as restored.
        let records: Vec<_> = self.apps.keys().cloned().collect();
        for (process, role) in records {
            if !processes.contains(&process) {
                self.apps.remove(&(process.clone(), role));
                self.route_retry
                    .remove(&format!("app:{}:{}:{role}", process.pid, process.started));
                events.push(format!(
                    "{} 已退出，其 Windows 偏好可能保留；无法通过已退出的 PID 还原",
                    process.name
                ));
                continue;
            }
            if c.guard_enabled
                && c.pin_apps
                && c.apps.iter().any(|n| n.eq_ignore_ascii_case(&process.name))
            {
                continue;
            }
            let key = format!("app:{}:{}:{role}", process.pid, process.started);
            let result = self.restore_app(b, &process, role, &key, now, c.retry_max_seconds);
            if let Err(e) = result {
                errors.push(format!("{} 偏好还原：{e:#}", process.name));
            }
        }
        if c.guard_enabled && c.pin_apps && target_online {
            for process in processes
                .iter()
                .filter(|p| c.apps.iter().any(|n| n.eq_ignore_ascii_case(&p.name)))
            {
                let mut good = true;
                for role in [0, 1, 2] {
                    let key = format!("app:{}:{}:{role}", process.pid, process.started);
                    if let Err(e) = self.pin_app(
                        b,
                        process,
                        role,
                        &c.output_input,
                        &key,
                        now,
                        c.retry_max_seconds,
                    ) {
                        good = false;
                        errors.push(format!("{} 偏好：{e:#}", process.name));
                    }
                }
                app_lines.push(format!(
                    "{} · Windows 偏好{}；实际输入未知",
                    process.name,
                    if good { "一致" } else { "检查未通过" }
                ));
            }
        }
        if app_lines.is_empty() {
            app_lines.push(
                if c.pin_apps && c.guard_enabled {
                    "未完成应用偏好检查或未发现目标进程；实际输入未知"
                } else {
                    "未启用 Windows 应用偏好守护；实际输入未知"
                }
                .into(),
            );
        }
        let health = if !errors.is_empty() {
            Health::error(errors.join("\n"))
        } else if routing && !target_online {
            Health::new(State::Waiting, "目标输入设备离线，等待重连")
        } else if routing {
            Health::new(
                State::Ready,
                "已核对 Windows 输入设置；应用实际输入须单独验证",
            )
        } else {
            Health::new(State::Disabled, "Windows 输入固定未启用")
        };
        (health, app_lines.join("\r\n"))
    }
    fn writable(&self, key: &str, now: Instant, b: &impl Backend) -> Result<()> {
        if !b.current() {
            bail!("新的设置正在应用，已取消旧设置写入");
        }
        if !self.route_retry.get(key).is_none_or(|r| r.ready(now)) {
            bail!("等待重试，继续检查设置");
        }
        Ok(())
    }
    fn record_result(
        &mut self,
        key: &str,
        now: Instant,
        max: u64,
        result: Result<()>,
    ) -> Result<()> {
        if result.is_ok() {
            self.route_retry.remove(key);
        } else {
            self.route_retry
                .entry(key.into())
                .or_default()
                .fail(now.max(Instant::now()), max);
        }
        result
    }
    #[allow(clippy::too_many_arguments)]
    fn pin_default(
        &mut self,
        b: &mut impl Backend,
        role: i32,
        target: &str,
        key: &str,
        now: Instant,
        max: u64,
        events: &mut Vec<String>,
    ) -> Result<()> {
        let old = b.default(role)?;
        if old == target {
            self.route_retry.remove(key);
            return Ok(());
        }
        self.writable(key, now, b)?;
        let record = self.defaults.entry(role).or_insert_with(|| Original {
            before: old.clone(),
            applied: Vec::new(),
        });
        // A failed SET may leave the previous value or the attempted value.
        // Preserve both until a subsequent observation resolves the outcome.
        record.applied.retain(|value| value == &old);
        record.applied.push(target.into());
        let result = b.set_default(role, target).and_then(|()| {
            if b.default(role)? != target {
                bail!("写入后的独立复查不一致");
            }
            Ok(())
        });
        self.record_result(key, now, max, result)?;
        events.push(format!(
            "已恢复 Windows {}输入",
            if role == 2 { "通信" } else { "默认" }
        ));
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn restore_default(
        &mut self,
        b: &mut impl Backend,
        role: i32,
        key: &str,
        now: Instant,
        max: u64,
        events: &mut Vec<String>,
    ) -> Result<()> {
        let Some(original) = self.defaults.get(&role).cloned() else {
            return Ok(());
        };
        let current = b.default(role)?;
        if original.applied.contains(&current) {
            self.writable(key, now, b)?;
            let result = b.set_default(role, &original.before).and_then(|()| {
                if b.default(role)? != original.before {
                    bail!("还原复查不一致");
                }
                Ok(())
            });
            self.record_result(key, now, max, result)?;
            events.push(format!("已还原 Windows 输入角色 {role}"));
        }
        self.defaults.remove(&role);
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn pin_app(
        &mut self,
        b: &mut impl Backend,
        process: &Process,
        role: i32,
        target: &str,
        key: &str,
        now: Instant,
        max: u64,
    ) -> Result<()> {
        let target = crate::audio::AppPolicy::full_id(target);
        let old = b.app_get(process, role)?;
        if old == target {
            self.route_retry.remove(key);
            return Ok(());
        }
        self.writable(key, now, b)?;
        let record = self
            .apps
            .entry((process.clone(), role))
            .or_insert_with(|| Original {
                before: old.clone(),
                applied: Vec::new(),
            });
        record.applied.retain(|value| value == &old);
        record.applied.push(target.clone());
        let result = b.app_set(process, role, &target).and_then(|()| {
            if b.app_get(process, role)? != target {
                bail!("偏好复查不一致");
            }
            Ok(())
        });
        self.record_result(key, now, max, result)
    }
    fn restore_app(
        &mut self,
        b: &mut impl Backend,
        process: &Process,
        role: i32,
        key: &str,
        now: Instant,
        max: u64,
    ) -> Result<()> {
        let original = self.apps[&(process.clone(), role)].clone();
        if original.applied.contains(&b.app_get(process, role)?) {
            self.writable(key, now, b)?;
            let result = b.app_set(process, role, &original.before).and_then(|()| {
                if b.app_get(process, role)? != original.before {
                    bail!("应用偏好还原复查不一致");
                }
                Ok(())
            });
            self.record_result(key, now, max, result)?;
        }
        self.apps.remove(&(process.clone(), role));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        observation: Option<AmdObservation>,
        defaults: [String; 3],
        writes: Vec<i32>,
        fail_read: bool,
        fail_write: Option<i32>,
        repairs: usize,
        current: bool,
        app_values: HashMap<(u32, i32), String>,
        app_writes: usize,
        cancel_during_read: bool,
    }
    impl Backend for Fake {
        fn current(&self) -> bool {
            self.current
        }
        fn amd_observe(&mut self, _: &str) -> Result<AmdObservation> {
            self.observation
                .clone()
                .ok_or_else(|| anyhow::anyhow!("unsupported AMD"))
        }
        fn amd_repair(&mut self, _: &str, _: &AmdObservation) -> Result<()> {
            self.repairs += 1;
            Ok(())
        }
        fn default(&mut self, role: i32) -> Result<String> {
            if self.cancel_during_read {
                self.current = false;
            }
            if self.fail_read {
                bail!("audio service unavailable");
            }
            Ok(self.defaults[role as usize].clone())
        }
        fn set_default(&mut self, role: i32, id: &str) -> Result<()> {
            self.writes.push(role);
            if self.fail_write == Some(role) {
                bail!("write failed");
            }
            self.defaults[role as usize] = id.into();
            Ok(())
        }
        fn app_get(&mut self, p: &Process, role: i32) -> Result<String> {
            Ok(self
                .app_values
                .get(&(p.pid, role))
                .cloned()
                .unwrap_or_default())
        }
        fn app_set(&mut self, p: &Process, role: i32, id: &str) -> Result<()> {
            self.app_writes += 1;
            self.app_values.insert((p.pid, role), id.into());
            Ok(())
        }
    }
    fn setup() -> (Engine, Config, Vec<Device>, Fake, Instant) {
        let c = Config {
            physical_input: "physical".into(),
            output_input: "virtual".into(),
            pin_default: true,
            pin_communications: true,
            ..Config::default()
        };
        let devices = vec![
            Device {
                id: "physical".into(),
                name: "USB".into(),
                active: true,
                amd: false,
            },
            Device {
                id: "virtual".into(),
                name: "AMD".into(),
                active: true,
                amd: true,
            },
        ];
        let fake = Fake {
            observation: None,
            defaults: ["old".into(), "old".into(), "old".into()],
            writes: vec![],
            fail_read: false,
            fail_write: None,
            repairs: 0,
            current: true,
            app_values: HashMap::new(),
            app_writes: 0,
            cancel_during_read: false,
        };
        (Engine::default(), c, devices, fake, Instant::now())
    }
    #[test]
    fn amd_failure_does_not_block_any_windows_role() {
        let (mut engine, c, devices, mut b, now) = setup();
        let cycle = engine.cycle(&c, &devices, &[], &mut b, now);
        assert_eq!(cycle.amd.state, State::Error);
        assert_eq!(b.defaults, ["virtual", "virtual", "virtual"]);
    }
    #[test]
    fn failed_console_role_does_not_block_communication_role() {
        let (mut engine, c, devices, mut b, now) = setup();
        b.fail_write = Some(0);
        assert_eq!(
            engine.cycle(&c, &devices, &[], &mut b, now).routes.state,
            State::Error
        );
        assert_eq!(b.defaults[2], "virtual");
    }
    #[test]
    fn failed_restore_read_retains_original_until_service_recovers() {
        let (mut engine, mut c, devices, mut b, now) = setup();
        engine.cycle(&c, &devices, &[], &mut b, now);
        c.guard_enabled = false;
        b.fail_read = true;
        assert_eq!(
            engine.cycle(&c, &devices, &[], &mut b, now).routes.state,
            State::Error
        );
        b.fail_read = false;
        engine.cycle(&c, &devices, &[], &mut b, now);
        assert_eq!(b.defaults, ["old", "old", "old"]);
    }
    #[test]
    fn pause_preserves_a_later_manual_device_change() {
        let (mut engine, mut c, devices, mut b, now) = setup();
        engine.cycle(&c, &devices, &[], &mut b, now);
        b.defaults[2] = "manual".into();
        c.guard_enabled = false;
        engine.cycle(&c, &devices, &[], &mut b, now);
        assert_eq!(b.defaults[2], "manual");
    }
    #[test]
    fn failed_target_switch_does_not_lose_previous_restore_value() {
        let (mut engine, mut c, mut devices, mut b, now) = setup();
        engine.cycle(&c, &devices, &[], &mut b, now);
        devices.push(Device {
            id: "new-target".into(),
            name: "second".into(),
            active: true,
            amd: false,
        });
        c.output_input = "new-target".into();
        b.fail_write = Some(2);
        engine.apply(&c);
        engine.cycle(&c, &devices, &[], &mut b, now);
        assert_eq!(b.defaults[2], "virtual");
        c.guard_enabled = false;
        b.fail_write = None;
        engine.apply(&c);
        engine.cycle(&c, &devices, &[], &mut b, now);
        assert_eq!(b.defaults[2], "old");
    }
    #[test]
    fn monitor_reads_during_retry_and_never_counts_flag_alone() {
        let (mut engine, c, devices, mut b, now) = setup();
        b.observation = Some(AmdObservation::Running {
            enabled: false,
            configured: true,
            capturing: false,
        });
        assert_eq!(
            engine.cycle(&c, &devices, &[], &mut b, now).amd.state,
            State::Repairing
        );
        b.observation = Some(AmdObservation::Running {
            enabled: true,
            configured: true,
            capturing: false,
        });
        engine.cycle(&c, &devices, &[], &mut b, now + Duration::from_secs(1));
        assert_eq!(engine.repaired, 0);
        assert_eq!(b.repairs, 1);
        b.observation = Some(AmdObservation::Running {
            enabled: true,
            configured: true,
            capturing: true,
        });
        assert_eq!(
            engine
                .cycle(&c, &devices, &[], &mut b, now + Duration::from_secs(2))
                .amd
                .state,
            State::Ready
        );
        assert_eq!(engine.repaired, 1);
    }
    #[test]
    fn pending_new_configuration_prevents_old_writes() {
        let (mut engine, c, devices, mut b, now) = setup();
        b.current = false;
        b.observation = Some(AmdObservation::Missing);
        engine.cycle(&c, &devices, &[], &mut b, now);
        assert!(b.writes.is_empty());
        assert_eq!(b.repairs, 0);
    }
    #[test]
    fn configuration_change_during_read_cancels_following_writes() {
        let (mut engine, c, devices, mut b, now) = setup();
        b.cancel_during_read = true;
        b.observation = Some(AmdObservation::Missing);
        engine.cycle(&c, &devices, &[], &mut b, now);
        assert!(b.writes.is_empty());
        assert_eq!(b.repairs, 0);
    }
    #[test]
    fn reused_pid_is_never_restored_from_another_process_lifetime() {
        let (mut engine, mut c, devices, mut b, now) = setup();
        c.pin_apps = true;
        c.apps = vec!["voice.exe".into()];
        let process = Process {
            pid: 99,
            name: "voice.exe".into(),
            started: 100,
        };
        engine.cycle(&c, &devices, std::slice::from_ref(&process), &mut b, now);
        assert_eq!(b.app_writes, 3);
        c.guard_enabled = false;
        let reused = Process {
            started: 200,
            ..process
        };
        engine.cycle(&c, &devices, &[reused], &mut b, now);
        assert_eq!(b.app_writes, 3);
        assert!(engine.apps.is_empty());
    }
    #[test]
    fn removed_app_stays_tracked_until_live_preferences_are_restored() {
        let (mut engine, mut c, devices, mut b, now) = setup();
        c.pin_apps = true;
        c.apps = vec!["voice.exe".into()];
        let process = Process {
            pid: 99,
            name: "voice.exe".into(),
            started: 100,
        };
        engine.cycle(&c, &devices, std::slice::from_ref(&process), &mut b, now);
        c.apps.clear();
        assert_eq!(engine.process_names(&c.apps), ["voice.exe"]);
        engine.cycle(&c, &devices, &[process], &mut b, now);
        assert_eq!(b.app_writes, 6);
        assert!(b.app_values.values().all(String::is_empty));
        assert!(engine.process_names(&c.apps).is_empty());
    }
    #[test]
    fn repeated_failures_are_bounded_and_virtual_reconnect_does_not_clear_amd_backoff() {
        let (mut engine, c, mut devices, mut b, now) = setup();
        engine.cycle(&c, &devices, &[], &mut b, now);
        for _ in 0..40 {
            engine.amd.retry.fail(now, 60);
        }
        devices[1].active = false;
        engine.cycle(&c, &devices, &[], &mut b, now);
        assert!(!engine.amd.retry.ready(now + Duration::from_secs(59)));
        assert!(engine.amd.retry.ready(now + Duration::from_secs(60)));
        devices[0].active = false;
        engine.cycle(&c, &devices, &[], &mut b, now);
        assert!(engine.amd.retry.ready(now));
    }
    #[test]
    fn disconnected_physical_device_never_switches_to_similar_name() {
        let (mut engine, c, mut devices, mut b, now) = setup();
        devices[0].id = "different-physical".into();
        b.observation = Some(AmdObservation::Missing);
        assert_eq!(
            engine.cycle(&c, &devices, &[], &mut b, now).amd.state,
            State::Waiting
        );
        assert_eq!(b.repairs, 0);
    }
}
