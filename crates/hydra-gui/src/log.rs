// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Leveled session log in `<app_dir>/logs/gui.log`.
//!
//! `[2026-08-18 06:36:12] [INFO ] start #3 https://…` — level filter comes
//! from `log_level` in config.toml (`debug`/`info`/`warn`/`error`, default
//! `info`) or the `HYDRA_LOG` environment variable, which wins. Failure to
//! log must never fail the operation being logged.

use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd)]
pub enum Level {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

static FILTER: AtomicU8 = AtomicU8::new(Level::Info as u8);

pub fn set_level(name: &str) {
    let lvl = match name.to_ascii_lowercase().as_str() {
        "debug" | "trace" | "verbose" => Level::Debug,
        "warn" | "warning" => Level::Warn,
        "error" => Level::Error,
        _ => Level::Info,
    };
    FILTER.store(lvl as u8, Ordering::Relaxed);
}

/// Apply config level, then let HYDRA_LOG override it for one-off debugging.
pub fn init(config_level: Option<&str>) {
    if let Some(l) = config_level {
        set_level(l);
    }
    if let Ok(env) = std::env::var("HYDRA_LOG") {
        set_level(&env);
    }
}

/// Where the session log lives. Exposed so Help > Logs can open exactly the
/// file this module writes, rather than a path spelled twice.
pub fn path() -> std::path::PathBuf {
    crate::model::app_dir().join("logs").join("gui.log")
}

fn write(level: Level, tag: &str, line: &str) {
    if (level as u8) < FILTER.load(Ordering::Relaxed) {
        return;
    }
    let file = path();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
    {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        // One formatted string, one write_all: `writeln!` with arguments
        // emits a write per fragment, and the extbus socket threads log
        // concurrently with the UI thread — that interleaves mid-line.
        let record = format!("[{ts}] [{tag}] {line}\n");
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock();
        let _ = f.write_all(record.as_bytes());
    }
}

pub fn debug(line: &str) {
    write(Level::Debug, "DEBUG", line);
}

pub fn info(line: &str) {
    write(Level::Info, "INFO ", line);
}

pub fn warn(line: &str) {
    write(Level::Warn, "WARN ", line);
}

pub fn error(line: &str) {
    write(Level::Error, "ERROR", line);
}

/// Back-compat alias for the original single-level call sites.
pub fn log(line: &str) {
    info(line);
}

/// The machine, once, at launch.
///
/// Every stream bug report starts with the same three round trips — which
/// build, which OS, was ffmpeg there — so the log answers them before they
/// are asked. It is deliberately ONE line plus the paths: a banner that
/// scrolls is a banner nobody reads.
///
/// Nothing here shells out. A launch path should not wait on a subprocess,
/// and every field is either a compile-time constant or a small file read.
pub fn banner() {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    info(&format!(
        "hydra-gui {} | {} {} | {} | {} core(s) | ffmpeg: {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        os_release().unwrap_or_else(|| "unknown release".into()),
        cpus,
        match hya_stream::hls::ffmpeg() {
            Some(p) => p.display().to_string(),
            // The single most common cause of "why is my HLS download a .ts
            // file" — worth stating at launch rather than per download.
            None => "not found".into(),
        },
    ));
    info(&format!("log: {}", path().display()));
    info(&format!("data: {}", crate::model::app_dir().display()));
}

/// Best-effort human-readable OS version.
#[cfg(target_os = "macos")]
fn os_release() -> Option<String> {
    let s = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist").ok()?;
    let tail = &s[s.find("<key>ProductVersion</key>")?..];
    let a = tail.find("<string>")? + "<string>".len();
    let b = tail[a..].find("</string>")?;
    Some(format!("macOS {}", &tail[a..a + b]))
}

/// `PRETTY_NAME` is the line every distribution agrees on, and the one that
/// distinguishes the snap-confined browsers case from the ordinary one.
#[cfg(target_os = "linux")]
fn os_release() -> Option<String> {
    let s = std::fs::read_to_string("/etc/os-release").ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim_matches('"').to_string())
}

#[cfg(target_os = "windows")]
fn os_release() -> Option<String> {
    // No subprocess and no registry crate: the environment already
    // distinguishes the NT line, and the build number is not worth a
    // dependency here.
    std::env::var("OS").ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn os_release() -> Option<String> {
    None
}

/// A URL safe to write into a file the user is invited to hand to someone.
///
/// Signed CDN URLs carry BEARER CREDENTIALS in the query string —
/// CloudFront's `Signature` + `Key-Pair-Id`, Akamai's `hdnts`, the `token`
/// on a hundred smaller CDNs. They are exactly the URLs the browser
/// extension hands over, and exactly the URLs an HLS or DASH manifest is
/// full of, so this log would otherwise collect them by the hundred.
///
/// That is a leak because Help > Logs exists: this file is meant to be
/// opened and pasted into a bug report. Whoever reads that report gets
/// working credentials for the user's session, still live if the signature
/// has not expired.
///
/// The path is what makes a log useful for diagnosis; the query is what
/// makes it dangerous. So the query goes, and its length stays — enough to
/// tell "no query" from "query elided" when reading a trace back.
pub fn redact(url: &str) -> String {
    match url.split_once('?') {
        Some((head, q)) => format!("{head}?<{} chars elided>", q.len()),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{os_release, redact};

    /// The banner's whole value is being specific, so a platform that
    /// silently reports "unknown release" is worth catching here rather
    /// than in a bug report that needed it.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn the_os_version_is_actually_discoverable_here() {
        let r = os_release().expect("this platform should report a release");
        assert!(!r.trim().is_empty(), "empty release string");
    }

    #[test]
    fn a_signed_url_loses_its_credentials_but_stays_diagnosable() {
        let signed = "https://cdn.example/hls/seg1.ts\
                      ?Expires=1750000000&Signature=abc123DEF&Key-Pair-Id=APKAIOSFODNN7";
        let out = redact(signed);

        // The credential is gone, in every part.
        assert!(!out.contains("abc123DEF"), "signature survived: {out}");
        assert!(!out.contains("APKAIOSFODNN7"), "key id survived: {out}");
        assert!(!out.contains("Expires"), "query survived: {out}");

        // What is left still says which segment, on which host.
        assert!(out.starts_with("https://cdn.example/hls/seg1.ts?"));
        assert!(out.contains("elided"));
    }

    #[test]
    fn a_plain_url_is_left_alone() {
        let plain = "https://cdn.example/hls/seg1.ts";
        assert_eq!(redact(plain), plain);
    }
}
