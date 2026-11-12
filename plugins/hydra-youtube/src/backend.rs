//! Runs yt-dlp, installing the official release on demand when the user opted in.
use base64::Engine;
use hya_plugin_sdk::{ErrorCode, ExecOutput, PluginError, Result, Value};
use serde_json::json;

use crate::env::Env;
use crate::invocation::{self, Arg};

pub const PROGRAM: &str = "yt-dlp";
/// Release installed on demand, and the oldest one `check` accepts silently.
pub const RELEASE: &str = "2026.10.26";

fn release_asset(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64" | "x86_64") => Some("yt-dlp_macos"),
        ("windows", "x86_64") => Some("yt-dlp.exe"),
        ("windows", "aarch64") => Some("yt-dlp_arm64.exe"),
        ("linux", "x86_64") => Some("yt-dlp_linux"),
        ("linux", "aarch64") => Some("yt-dlp_linux_aarch64"),
        _ => None,
    }
}

fn checksum_for<'a>(sums: &'a str, asset: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let digest = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == asset).then_some(digest)
    })
}

fn release_number(version: &str) -> Option<Vec<u64>> {
    version
        .trim()
        .split('.')
        .map(|part| part.parse().ok())
        .collect()
}

fn older_than_release(version: &str) -> bool {
    match (release_number(version), release_number(RELEASE)) {
        (Some(found), Some(wanted)) => found < wanted,
        _ => false,
    }
}

/// The host reports a replaced binary as missing; installing over it is refused.
fn approval_is_stale(error: &PluginError) -> bool {
    error.message.contains("changed since you approved")
}

fn stale_approval() -> PluginError {
    PluginError::new(
        ErrorCode::ToolMissing,
        "yt-dlp was updated after you approved it. Approve the new program under this plugin's permissions, or run `hydra plugin grant hydra.youtube exec:yt-dlp`.",
    )
}

fn install(env: &impl Env, missing: PluginError) -> Result<()> {
    if approval_is_stale(&missing) {
        return Err(stale_approval());
    }
    let opted_in = env
        .settings()?
        .get("download_backend")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !opted_in {
        return Err(PluginError::new(
            ErrorCode::ToolMissing,
            format!(
                "{} Install yt-dlp, or enable 'Download yt-dlp if it is missing' in this plugin's settings.",
                missing.message
            ),
        ));
    }
    let platform = env.call("platform", json!({}))?;
    let os = platform["os"].as_str().unwrap_or("");
    let asset = release_asset(os, platform["arch"].as_str().unwrap_or("")).ok_or_else(|| {
        PluginError::new(
            ErrorCode::Unsupported,
            "No official yt-dlp build exists for this platform; install yt-dlp manually.",
        )
    })?;
    let base = format!("https://github.com/yt-dlp/yt-dlp/releases/download/{RELEASE}");
    let response = env.call("http", json!({"url": format!("{base}/SHA2-256SUMS")}))?;
    if response["status"] != 200 {
        return Err(PluginError::new(
            ErrorCode::Network,
            format!(
                "Could not fetch the yt-dlp {RELEASE} checksums from GitHub (HTTP {}).",
                response["status"]
            ),
        ));
    }
    let invalid =
        |e: &dyn std::fmt::Display| PluginError::new(ErrorCode::InvalidReply, e.to_string());
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(response["body_b64"].as_str().unwrap_or(""))
        .map_err(|e| invalid(&e))?;
    let sums = String::from_utf8(bytes).map_err(|e| invalid(&e))?;
    let digest = checksum_for(&sums, asset).ok_or_else(|| {
        PluginError::new(
            ErrorCode::InvalidReply,
            format!("yt-dlp {RELEASE} publishes no checksum for {asset}"),
        )
    })?;
    env.log("info", &format!("Installing yt-dlp {RELEASE} for {os}"));
    env.call(
        "program_install",
        json!({"program": PROGRAM, "url": format!("{base}/{asset}"), "sha256": digest}),
    )?;
    Ok(())
}

/// Runs yt-dlp once; a missing program is installed first when the user allowed it.
///
/// # Errors
/// The host's `exec` error, or why the program could not be installed.
pub fn run(env: &impl Env, args: &[Arg]) -> Result<ExecOutput> {
    let line = invocation::command_line(args);
    match env.exec(PROGRAM, &line) {
        Err(error) if error.code == ErrorCode::ToolMissing => {
            install(env, error)?;
            env.exec(PROGRAM, &line)
        }
        other => other,
    }
}

/// Confirms yt-dlp starts, and flags a release too old for today's YouTube.
///
/// # Errors
/// Why yt-dlp is unavailable.
pub fn check(env: &impl Env) -> Result<()> {
    let output = run(env, &invocation::version())?;
    let stdout = base64::engine::general_purpose::STANDARD
        .decode(&output.stdout_b64)
        .unwrap_or_default();
    let version = String::from_utf8_lossy(&stdout).trim().to_owned();
    if older_than_release(&version) {
        env.log(
            "warn",
            &format!(
                "yt-dlp {version} is older than {RELEASE}. YouTube changes often break old releases: update yt-dlp, then approve the new program in this plugin's permissions."
            ),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::fake::{failure, output, FakeEnv};

    const SUMS: &str = "aaa  yt-dlp\nbbb *yt-dlp_macos\nccc  yt-dlp_linux\n";

    fn sums_reply() -> serde_json::Value {
        json!({
            "status": 200,
            "body_b64": base64::engine::general_purpose::STANDARD.encode(SUMS),
        })
    }

    fn missing() -> Result<ExecOutput> {
        failure(ErrorCode::ToolMissing, "`yt-dlp` was not found")
    }

    #[test]
    fn an_installed_backend_runs_once_and_touches_nothing_else() {
        let env = FakeEnv::with_exec([output("2026.10.26\n", "")]);
        check(&env).unwrap();
        assert_eq!(env.executed.borrow().len(), 1);
        assert!(env.called.borrow().is_empty());
        assert!(env.logs.borrow().is_empty());
    }

    #[test]
    fn a_missing_backend_without_opt_in_explains_both_ways_out() {
        let env = FakeEnv::with_exec([missing()]);
        let error = run(&env, &invocation::version()).unwrap_err();
        assert_eq!(error.code, ErrorCode::ToolMissing);
        assert!(error.message.contains("was not found"));
        assert!(error.message.contains("Download yt-dlp if it is missing"));
        assert!(env.called.borrow().is_empty());
    }

    #[test]
    fn opting_in_installs_the_verified_release_asset_then_retries() {
        let env = FakeEnv::with_exec([missing(), output("2026.10.26\n", "")])
            .set("download_backend", Value::Bool(true))
            .replying([
                json!({"os": "macos", "arch": "aarch64"}),
                sums_reply(),
                json!({}),
            ]);
        check(&env).unwrap();
        let called = env.called.borrow();
        assert_eq!(called[1].0, "http");
        assert_eq!(
            called[1].1["url"],
            format!("https://github.com/yt-dlp/yt-dlp/releases/download/{RELEASE}/SHA2-256SUMS")
        );
        assert_eq!(called[2].0, "program_install");
        assert_eq!(called[2].1["sha256"], "bbb");
        assert_eq!(
            called[2].1["url"],
            format!("https://github.com/yt-dlp/yt-dlp/releases/download/{RELEASE}/yt-dlp_macos")
        );
        assert_eq!(env.executed.borrow().len(), 2);
        assert!(env.logged("info", "Installing yt-dlp"));
    }

    #[test]
    fn installation_stops_at_the_first_thing_it_cannot_trust() {
        let opted_in =
            || FakeEnv::with_exec([missing()]).set("download_backend", Value::Bool(true));
        let unsupported = opted_in().replying([json!({"os": "freebsd", "arch": "x86_64"})]);
        assert_eq!(
            run(&unsupported, &invocation::version()).unwrap_err().code,
            ErrorCode::Unsupported
        );
        let unreachable = opted_in().replying([
            json!({"os": "linux", "arch": "x86_64"}),
            json!({"status": 503, "body_b64": ""}),
        ]);
        let error = run(&unreachable, &invocation::version()).unwrap_err();
        assert_eq!(error.code, ErrorCode::Network);
        assert!(error.message.contains("503"));
        let unlisted =
            opted_in().replying([json!({"os": "windows", "arch": "aarch64"}), sums_reply()]);
        let error = run(&unlisted, &invocation::version()).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidReply);
        assert!(error.message.contains("yt-dlp_arm64.exe"));
        for env in [unsupported, unreachable, unlisted] {
            assert!(env
                .called
                .borrow()
                .iter()
                .all(|(name, _)| name != "program_install"));
        }
    }

    #[test]
    fn an_updated_binary_asks_for_approval_instead_of_a_reinstall() {
        let env = FakeEnv::with_exec([failure(
            ErrorCode::ToolMissing,
            "`/opt/homebrew/bin/yt-dlp` changed since you approved it — approve again",
        )])
        .set("download_backend", Value::Bool(true));
        let error = run(&env, &invocation::version()).unwrap_err();
        assert_eq!(error.code, ErrorCode::ToolMissing);
        assert!(error
            .message
            .contains("hydra plugin grant hydra.youtube exec:yt-dlp"));
        assert!(env.called.borrow().is_empty());
    }

    #[test]
    fn failures_other_than_a_missing_program_are_passed_through() {
        let env = FakeEnv::with_exec([failure(ErrorCode::ToolFailed, "exited with 1: boom")]);
        let error = run(&env, &invocation::version()).unwrap_err();
        assert_eq!(error.code, ErrorCode::ToolFailed);
        assert_eq!(env.executed.borrow().len(), 1);
    }

    #[test]
    fn check_warns_only_about_releases_older_than_the_pinned_one() {
        for (version, warns) in [
            ("2026.08.19", true),
            ("2025.12.08", true),
            ("2026.10.26", false),
            ("2026.10.26.232945", false),
            ("2026.11.01", false),
            ("fixture-1.0", false),
        ] {
            let env = FakeEnv::with_exec([output(&format!("{version}\n"), "")]);
            check(&env).unwrap();
            assert_eq!(env.logged("warn", "is older than"), warns, "{version}");
        }
    }

    #[test]
    fn release_assets_cover_supported_desktops() {
        assert_eq!(release_asset("macos", "aarch64"), Some("yt-dlp_macos"));
        assert_eq!(release_asset("macos", "x86_64"), Some("yt-dlp_macos"));
        assert_eq!(release_asset("windows", "x86_64"), Some("yt-dlp.exe"));
        assert_eq!(
            release_asset("windows", "aarch64"),
            Some("yt-dlp_arm64.exe")
        );
        assert_eq!(release_asset("linux", "x86_64"), Some("yt-dlp_linux"));
        assert_eq!(
            release_asset("linux", "aarch64"),
            Some("yt-dlp_linux_aarch64")
        );
        assert_eq!(release_asset("linux", "riscv64"), None);
    }
}
