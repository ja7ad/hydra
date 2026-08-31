// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! hydra-updater: the finisher that completes an update after Hydra exits.
//!
//! The GUI downloads and extracts the new release itself, copies THIS binary
//! out of the new bundle into the staging directory (so it never overwrites
//! itself while running), launches it detached, and exits. This process then:
//!
//! 1. waits a beat for the parent to finish exiting,
//! 2. copies the new files over the installed ones (retrying while the old
//!    executables are still locked — the retry loop is the "wait for exit"
//!    on Windows, where a running exe cannot be replaced but can be renamed),
//! 3. re-runs itself through the platform's authorisation prompt when the
//!    install turns out to be root-owned (`--apply-only`, no relaunch: the
//!    app must come back as the user, never as root),
//! 4. relaunches the application,
//! 5. logs everything to `<staging>/update.log` for post-mortems.
//!
//! It is deliberately headless: by the time it runs there is no UI process
//! left to host a window, and a failed swap leaves the old install intact
//! (each file is renamed aside before its replacement is copied in).

use clap::Parser;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(name = "hydra-updater", about = "Hydra update finisher", version)]
struct Args {
    /// Directory holding the new release's files (an extracted bundle root).
    #[arg(
        long = "src-dir",
        value_name = "DIR",
        required_unless_present = "src_file",
        conflicts_with = "src_file"
    )]
    src_dir: Option<PathBuf>,

    /// The new `.AppImage`, for an AppImage install. Paired with
    /// `--appimage`: one downloaded file replacing one installed file, with
    /// no bundle to extract and no directory to walk.
    #[arg(long = "src-file", value_name = "FILE", requires = "appimage")]
    src_file: Option<PathBuf>,

    /// Directory of the running install to update (where hydra-gui lives).
    #[arg(
        long = "install-dir",
        value_name = "DIR",
        required_unless_present = "appimage"
    )]
    install_dir: Option<PathBuf>,

    /// The installed `.AppImage` to replace. An AppImage install is one
    /// file wherever the user keeps it, not a directory of binaries, so it
    /// is named outright rather than derived from the running executable
    /// (which lives in a mount that is gone by the time this runs).
    #[arg(long = "appimage", value_name = "FILE", requires = "src_file")]
    appimage: Option<PathBuf>,

    /// Version the new files carry, stamped into a macOS bundle's Info.plist.
    /// Not `--version`: clap owns that one, and it prints this binary's own
    /// version rather than the release it is installing.
    #[arg(long = "app-version", value_name = "VERSION")]
    app_version: Option<String>,

    /// Executable to launch once the swap is done.
    #[arg(long = "relaunch", value_name = "EXE")]
    relaunch: Option<PathBuf>,

    /// Extra arguments for the relaunched executable.
    #[arg(long = "relaunch-arg", value_name = "ARG")]
    relaunch_args: Vec<String>,

    /// Swap the files and stop: no waiting for a parent, no authorisation
    /// retry, no relaunch. This is how the unprivileged run re-invokes the
    /// finisher as root.
    #[arg(long = "apply-only")]
    apply_only: bool,
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let mut log = Log::open();
    log.line(&format!(
        "hydra-updater {} starting: src={} install={}{}",
        env!("CARGO_PKG_VERSION"),
        show(args.src_file.as_deref().or(args.src_dir.as_deref())),
        show(args.appimage.as_deref().or(args.install_dir.as_deref())),
        if args.apply_only { " (apply-only)" } else { "" }
    ));

    if !args.apply_only {
        // Give the parent a moment to leave its main loop; file locks
        // (Windows) are then handled by the per-file retry inside `apply`.
        std::thread::sleep(std::time::Duration::from_millis(1200));
    }

    let opts = hya_updater::ApplyOptions {
        version: args.app_version.clone(),
    };
    // An AppImage is replaced as one file; every other install is a
    // directory of binaries the new bundle is copied over. clap has already
    // established that exactly one of the two pairs was given.
    let outcome = match (
        &args.appimage,
        &args.src_file,
        &args.src_dir,
        &args.install_dir,
    ) {
        (Some(dest), Some(src), _, _) => hya_updater::replace_appimage(src, dest),
        (_, _, Some(src), Some(dir)) => hya_updater::apply_with(src, dir, &opts),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "need --src-dir with --install-dir, or --src-file with --appimage",
        )),
    };
    match outcome {
        Ok(report) => {
            log.line(&format!("replaced: {}", report.replaced.join(", ")));
            if !report.skipped.is_empty() {
                log.line(&format!("skipped:  {}", report.skipped.join(", ")));
            }
            for note in &report.notes {
                log.line(&format!("bundle:   {note}"));
            }
        }
        // Root owns the install (a tarball unpacked with sudo, an app copied
        // by another admin). The files are still ours to replace — with the
        // user's authorisation, which is what the elevated re-run asks for.
        Err(e) if !args.apply_only && e.kind() == std::io::ErrorKind::PermissionDenied => {
            log.line(&format!("update needs authorisation: {e}"));
            if let Err(e) = elevate(&args, &mut log) {
                log.line(&format!("update FAILED: {e}"));
                relaunch(&args, &mut log);
                return std::process::ExitCode::FAILURE;
            }
        }
        Err(e) => {
            log.line(&format!("update FAILED: {e}"));
            // Relaunch anyway: the rename-aside dance rolls back per file,
            // so whatever is in the install dir is runnable.
            relaunch(&args, &mut log);
            return std::process::ExitCode::FAILURE;
        }
    }

    relaunch(&args, &mut log);
    log.line("done");
    std::process::ExitCode::SUCCESS
}

/// Re-run this same binary as root, for the file swap only.
///
/// `--apply-only` is what keeps the relaunch on this side of the prompt: an
/// app relaunched by the elevated process would run as root and write
/// root-owned files into the user's config directory.
fn elevate(args: &Args, log: &mut Log) -> std::io::Result<()> {
    let Some(how) = hya_updater::elevation() else {
        return Err(std::io::Error::other(
            "no authorisation helper on this system (osascript, pkexec)",
        ));
    };
    let me = std::env::current_exe()?;
    let mut argv: Vec<&OsStr> = vec![me.as_os_str(), OsStr::new("--apply-only")];
    match (&args.appimage, &args.src_file) {
        (Some(dest), Some(src)) => {
            argv.push(OsStr::new("--src-file"));
            argv.push(src.as_os_str());
            argv.push(OsStr::new("--appimage"));
            argv.push(dest.as_os_str());
        }
        _ => {
            let (Some(src), Some(dir)) = (&args.src_dir, &args.install_dir) else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "nothing to re-run: no source and install directory",
                ));
            };
            argv.push(OsStr::new("--src-dir"));
            argv.push(src.as_os_str());
            argv.push(OsStr::new("--install-dir"));
            argv.push(dir.as_os_str());
        }
    }
    if let Some(v) = &args.app_version {
        argv.push(OsStr::new("--app-version"));
        argv.push(OsStr::new(v));
    }
    log.line(&format!("asking for authorisation via {how:?}"));
    hya_updater::run_elevated(&how, &argv)?;
    log.line("elevated swap finished");
    Ok(())
}

fn relaunch(args: &Args, log: &mut Log) {
    if args.apply_only {
        return;
    }
    let Some(exe) = &args.relaunch else {
        return;
    };
    // A macOS app is launched through its bundle, not its executable:
    // `open` hands it to LaunchServices, which is what gives the process its
    // Dock icon, its product name and the TCC identity the user granted
    // Downloads access to.
    if let Some(bundle) = hya_updater::app_bundle_root(exe) {
        let mut cmd = std::process::Command::new("/usr/bin/open");
        cmd.arg("-a").arg(&bundle);
        if !args.relaunch_args.is_empty() {
            cmd.arg("--args").args(&args.relaunch_args);
        }
        match cmd.status() {
            Ok(s) if s.success() => {
                log.line(&format!("relaunched {}", bundle.display()));
                return;
            }
            Ok(s) => log.line(&format!("open {} exited with {s}", bundle.display())),
            Err(e) => log.line(&format!("open {} failed: {e}", bundle.display())),
        }
        // Fall through: running the executable directly still works, it just
        // skips LaunchServices.
    }
    // An AppImage has no install directory; the image's own directory is
    // the closest thing, and the relaunched process gets $OWD from the
    // runtime anyway.
    let dir: Option<&Path> = args
        .install_dir
        .as_deref()
        .or_else(|| args.appimage.as_deref().and_then(Path::parent));
    let mut cmd = std::process::Command::new(exe);
    cmd.args(&args.relaunch_args);
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    match cmd.spawn() {
        Ok(_) => log.line(&format!("relaunched {}", exe.display())),
        Err(e) => log.line(&format!("relaunch failed: {e}")),
    }
}

/// A path for the log, or `-` when the run did not carry one.
fn show(p: Option<&Path>) -> String {
    p.map(|p| p.display().to_string())
        .unwrap_or_else(|| "-".into())
}

/// Append-only log in the staging directory; the updater has no other way to
/// report (its parent is gone and it may have no console).
struct Log(Option<std::fs::File>);

impl Log {
    fn open() -> Log {
        let dir = hya_updater::staging_dir();
        let _ = std::fs::create_dir_all(&dir);
        Log(std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("update.log"))
            .ok())
    }

    fn line(&mut self, msg: &str) {
        if let Some(f) = &mut self.0 {
            let _ = writeln!(f, "[{}] {msg}", std::process::id());
        }
    }
}
