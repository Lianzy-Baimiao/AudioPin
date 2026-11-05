use crate::{config::Config, platform::hidden_command};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{
    io::Read,
    process::Stdio,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
}
pub fn check(c: &Config) -> Result<String> {
    c.validate()?;
    let path = format!("repos/{}/releases/latest", c.update_repository);
    let text = if c.use_gh_auth {
        // gh keeps authentication in its credential store. Tokens never enter config/logs.
        gh_json(&path).context(
            "私有仓库检查失败。请先安装 GitHub CLI 并执行 gh auth login；也可关闭私有仓库认证",
        )?
    } else {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .build()
            .into();
        let mut response = agent
            .get(&format!("https://api.github.com/{path}"))
            .header("Accept", "application/vnd.github+json")
            .header(
                "User-Agent",
                concat!("AudioPin/", env!("CARGO_PKG_VERSION")),
            )
            .call()
            .context("GitHub 请求失败；404 可能表示尚无 Release 或仓库需要认证")?;
        response
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_string()?
    };
    let release: Release = serde_json::from_str(&text).context("GitHub 返回格式无效")?;
    if release.draft || release.prerelease {
        bail!("未找到稳定版本");
    }
    let remote = semver::Version::parse(release.tag_name.trim_start_matches('v'))?;
    let local = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    let expected = format!("https://github.com/{}/releases/", c.update_repository);
    if !release.html_url.starts_with(&expected) {
        bail!("Release 地址不属于配置的 GitHub 仓库");
    }
    if remote > local {
        Ok(format!(
            "发现新版本 {remote}（当前 {local}）。点击“打开发布页”下载。"
        ))
    } else {
        Ok(format!("已是最新稳定版 {local}。GitHub 最新版本：{remote}"))
    }
}
fn gh_json(path: &str) -> Result<String> {
    let mut child = hidden_command("gh")
        .args(["api", "--hostname", "github.com", path])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        out.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        err.take(8192).read_to_end(&mut bytes).map(|_| bytes)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("GitHub 检查超时，请检查网络后重试");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let output = reader
        .join()
        .map_err(|_| anyhow::anyhow!("读取 GitHub 响应失败"))??;
    let _ = errors.join();
    if !status.success() {
        bail!("GitHub CLI 请求失败：请检查登录、仓库权限或是否已有 Release");
    }
    if output.len() > 1024 * 1024 {
        bail!("GitHub 响应过大");
    }
    Ok(String::from_utf8(output)?)
}
