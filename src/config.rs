use crate::audio::Device;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub guard_enabled: bool,
    pub amd_enabled: bool,
    pub physical_input: String,
    /// Last known name of `physical_input`, shown while Windows lacks it.
    pub physical_input_name: String,
    pub output_input: String,
    /// Last known name of `output_input`.
    pub output_input_name: String,
    pub pin_default: bool,
    pub pin_communications: bool,
    pub poll_seconds: u64,
    pub retry_max_seconds: u64,
    pub notifications: bool,
    pub notification_cooldown_seconds: u64,
    pub autostart: bool,
    pub start_minimized: bool,
    pub close_to_tray: bool,
    pub restart_amd_if_missing: bool,
    pub reload_amd_panel: bool,
    pub apps: Vec<String>,
    pub pin_apps: bool,
    pub update_repository: String,
    pub auto_check_updates: bool,
    pub use_gh_auth: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: 1,
            guard_enabled: true,
            amd_enabled: true,
            physical_input: String::new(),
            physical_input_name: String::new(),
            output_input: String::new(),
            output_input_name: String::new(),
            pin_default: false,
            pin_communications: false,
            poll_seconds: 2,
            retry_max_seconds: 60,
            notifications: true,
            notification_cooldown_seconds: 10,
            autostart: false,
            start_minimized: true,
            close_to_tray: true,
            restart_amd_if_missing: true,
            reload_amd_panel: true,
            apps: Vec::new(),
            pin_apps: false,
            update_repository: "Lianzy-Baimiao/AudioPin".into(),
            auto_check_updates: false,
            use_gh_auth: true,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("配置版本不受支持：{}", self.schema_version);
        }
        if !(1..=60).contains(&self.poll_seconds) {
            bail!("检查间隔应为 1–60 秒");
        }
        if !(5..=600).contains(&self.retry_max_seconds) {
            bail!("最大重试间隔应为 5–600 秒");
        }
        if self.notification_cooldown_seconds > 3600 {
            bail!("通知间隔不能大于 3600 秒");
        }
        for id in [&self.physical_input, &self.output_input] {
            if !id.is_empty()
                && (!id.starts_with("{0.0.1.")
                    || id.encode_utf16().count() >= 128
                    || id.contains('\0'))
            {
                bail!("无效的麦克风设备 ID");
            }
        }
        if self.amd_enabled
            && !self.physical_input.is_empty()
            && self.physical_input == self.output_input
        {
            bail!("AMD 原始输入与降噪输出不能是同一设备（会造成音频回路）");
        }
        if (self.pin_default || self.pin_communications || self.pin_apps)
            && self.output_input.is_empty()
        {
            bail!("启用输入固定前，请先在“设备与路由”选择提供给语音软件的输入");
        }
        for app in &self.apps {
            if app.len() > 128
                || !app.to_ascii_lowercase().ends_with(".exe")
                || app.contains(['/', '\\', '\0', ':'])
            {
                bail!("应用必须填写进程文件名，例如 YY.exe：{app}");
            }
        }
        let parts: Vec<_> = self.update_repository.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|p| {
                p.is_empty()
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            })
        {
            bail!("更新仓库格式应为 owner/repo");
        }
        Ok(())
    }
    /// Keeps saved selections on the endpoints Windows recreated for them.
    /// A recreated endpoint (e.g. after a Windows Update restart) lists the
    /// IDs it replaced. Only that record is followed, never a similar name.
    /// Also remembers each selection's name for display while Windows lacks
    /// it. Returns a log message per followed selection.
    pub fn follow_devices(&mut self, devices: &[Device]) -> Vec<String> {
        let mut followed = Vec::new();
        for (role, id, name) in [
            (
                "AMD 原始麦克风",
                &mut self.physical_input,
                &mut self.physical_input_name,
            ),
            (
                "Windows 输入",
                &mut self.output_input,
                &mut self.output_input_name,
            ),
        ] {
            if id.is_empty() {
                name.clear();
                continue;
            }
            let device = devices.iter().find(|d| d.id == *id);
            if !device.is_some_and(|d| d.active) {
                if let Some(next) = successor(id, devices) {
                    followed.push(format!(
                        "Windows 已更换{role}的设备标识，已自动跟随：{}",
                        next.name
                    ));
                    id.clone_from(&next.id);
                    name.clone_from(&next.name);
                    continue;
                }
            }
            if let Some(device) = device {
                name.clone_from(&device.name);
            }
        }
        followed
    }
    pub fn load() -> Result<Self> {
        let path = data_dir().join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let config: Self =
            toml::from_str(&fs::read_to_string(path)?).context("读取配置失败，原文件已保留")?;
        config.validate()?;
        Ok(config)
    }
    pub fn save(&self) -> Result<()> {
        self.validate()?;
        fs::create_dir_all(data_dir())?;
        let tmp = data_dir().join("config.toml.tmp");
        fs::write(&tmp, toml::to_string_pretty(self)?)?;
        let src = crate::platform::wide(tmp.as_os_str());
        let dst = crate::platform::wide(data_dir().join("config.toml").as_os_str());
        unsafe {
            use windows_sys::Win32::Storage::FileSystem::*;
            if MoveFileExW(
                src.as_ptr(),
                dst.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        Ok(())
    }
}
pub fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into())).join("AudioPin")
}
/// The endpoint that replaced `id`, if Windows recorded exactly one.
fn successor<'a>(id: &str, devices: &'a [Device]) -> Option<&'a Device> {
    let replaces = |d: &Device, old: &str| {
        d.id != old && d.former_ids.iter().any(|f| f.eq_ignore_ascii_case(old))
    };
    let candidates: Vec<_> = devices.iter().filter(|d| replaces(d, id)).collect();
    // Windows keeps the whole history, so an intermediate endpoint that was
    // replaced again can also list `id`. Follow only the newest one.
    let mut newest = candidates
        .iter()
        .filter(|d| !candidates.iter().any(|n| replaces(n, &d.id)));
    match (newest.next(), newest.next()) {
        (Some(d), None) => Some(d),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_busy_loop_and_feedback_loop() {
        let mut c = Config {
            poll_seconds: 0,
            ..Config::default()
        };
        assert!(c.validate().is_err());
        c.poll_seconds = 2;
        c.physical_input = "{0.0.1.00000000}.{abc}".into();
        c.output_input = c.physical_input.clone();
        assert!(c.validate().is_err());
    }
    #[test]
    fn roundtrip_and_reject_unknown_settings() {
        let c = Config::default();
        let s = toml::to_string(&c).unwrap();
        toml::from_str::<Config>(&s).unwrap().validate().unwrap();
        assert!(toml::from_str::<Config>("poll_second = 0").is_err());
    }
    fn device(n: u32, active: bool, former: &[u32]) -> Device {
        Device {
            id: endpoint(n),
            name: format!("麦克风 {n}"),
            active,
            amd: false,
            former_ids: former.iter().map(|&f| endpoint(f).to_uppercase()).collect(),
        }
    }
    fn endpoint(n: u32) -> String {
        format!("{{0.0.1.00000000}}.{{{n:08x}-c779-41b9-a342-6c00e0c3eb85}}")
    }
    #[test]
    fn follows_only_the_endpoint_windows_recorded_as_replacement() {
        let mut c = Config {
            physical_input: endpoint(1),
            ..Config::default()
        };
        let lookalike = Device {
            name: "麦克风 1".into(),
            ..device(9, true, &[])
        };
        assert!(c
            .follow_devices(std::slice::from_ref(&lookalike))
            .is_empty());
        assert_eq!(c.physical_input, endpoint(1), "never follow a similar name");
        let followed = c.follow_devices(&[lookalike, device(2, true, &[7, 1])]);
        assert_eq!(followed.len(), 1);
        assert_eq!(c.physical_input, endpoint(2));
        assert_eq!(c.physical_input_name, "麦克风 2");
        c.validate().unwrap();
        assert!(c.follow_devices(&[]).is_empty());
        assert_eq!(
            c.physical_input_name, "麦克风 2",
            "remembered while missing"
        );
    }
    #[test]
    fn follows_newest_replacement_and_never_an_active_or_ambiguous_one() {
        let mut c = Config {
            physical_input: endpoint(1),
            output_input: endpoint(5),
            ..Config::default()
        };
        // 1 was replaced by 2, then 2 by 3; Windows lists the whole history.
        let devices = [
            device(2, false, &[1]),
            device(3, true, &[2, 1]),
            device(5, true, &[]),
            device(6, true, &[5]),
        ];
        assert_eq!(c.follow_devices(&devices).len(), 1);
        assert_eq!(c.physical_input, endpoint(3));
        assert_eq!(c.output_input, endpoint(5), "an active selection stays");
        assert_eq!(c.output_input_name, "麦克风 5");
        c.physical_input = endpoint(1);
        assert!(c
            .follow_devices(&[device(3, true, &[1]), device(4, true, &[1])])
            .is_empty());
        assert_eq!(c.physical_input, endpoint(1), "two claimants are ambiguous");
    }
}
