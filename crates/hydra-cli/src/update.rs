// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! `hydra update`: report whether a newer release exists, with its notes and
//! the download link for this OS/arch. Deliberately report-only — the CLI is
//! frequently installed by package managers or scripts that own the binary,
//! so replacing itself behind their back would be wrong. The GUI has the
//! self-updating flow.
//!
//! `--beta` switches to the pre-release channel: the newest `-rc` release
//! while it is ahead of stable, the same rule the GUI's "Download Beta
//! channel" setting and `install.sh --beta` follow.
//!
//! The endpoint honours `HYDRA_UPDATE_API`, so the whole command is testable
//! against `cargo run -p hya-updater --example mock_server`.

use std::process::ExitCode;

pub async fn run(json: bool, beta: bool) -> ExitCode {
    let current = env!("CARGO_PKG_VERSION");
    let ua = format!("hydra-cli/{current}");
    let rel = match hya_updater::check_channel(&ua, beta).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("hydra: update check failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let latest = rel.version().to_string();
    // GitHub's generated notes open with an HTML comment naming the config
    // that produced them; it is not release content.
    let notes = hya_updater::clean_notes(&rel.body);
    let newer = hya_updater::is_newer(&latest, current);
    // Inside an AppImage the standalone CLI tarball is the wrong answer:
    // this binary lives in the image, and the image is what gets replaced.
    let appimage = hya_updater::appimage_path();
    let asset = match &appimage {
        Some(_) => rel.appimage_asset(),
        None => rel.cli_asset(),
    };

    if json {
        let out = serde_json::json!({
            "current": current,
            "latest": latest,
            "channel": if beta { "beta" } else { "stable" },
            "prerelease": rel.prerelease,
            "update_available": newer,
            "release_page": rel.html_url,
            "published_at": rel.published_at,
            "asset": asset.map(|a| serde_json::json!({
                "name": a.name,
                "url": a.browser_download_url,
                "size": a.size,
            })),
            // The deb/rpm for this machine, when a package manager owns it.
            "package": package_managed().then(|| rel.package_asset()).flatten().map(|a| serde_json::json!({
                "name": a.name,
                "url": a.browser_download_url,
                "size": a.size,
            })),
            "notes": notes,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return ExitCode::SUCCESS;
    }

    let channel = if beta { "beta" } else { "stable" };
    if !newer {
        println!("hydra {current} is up to date ({channel} channel: {latest}).");
        return ExitCode::SUCCESS;
    }

    let kind = if rel.prerelease {
        "A new release candidate of hydra is available"
    } else {
        "A new version of hydra is available"
    };
    println!("{kind}: {latest} (you have {current})");
    if !rel.published_at.is_empty() {
        // `2026-08-19T00:00:00Z` — the date part reads fine on its own.
        let date = rel.published_at.split('T').next().unwrap_or("");
        println!("Published: {date}");
    }
    if !notes.is_empty() {
        println!();
        println!("Release notes:");
        for line in notes.lines() {
            println!("  {line}");
        }
    }
    println!();
    if !rel.html_url.is_empty() {
        println!("Release page: {}", rel.html_url);
    }
    // When a package manager owns this copy, the archive is not what the
    // user wants: the deb or rpm is, and only it may rewrite /usr/bin.
    if package_managed() {
        if let Some(pkg) = rel.package_asset() {
            println!("Package: {}", pkg.browser_download_url);
        }
    }
    match asset {
        Some(a) => println!(
            "Download ({}): {}",
            platform(&appimage),
            a.browser_download_url
        ),
        None => println!(
            "No prebuilt archive for {} in this release; see the release page.",
            platform(&appimage)
        ),
    }
    ExitCode::SUCCESS
}

/// How this build names the platform it wants an asset for: an AppImage by
/// its own architecture spelling, everything else by the release archives'.
fn platform(appimage: &Option<std::path::PathBuf>) -> String {
    match appimage {
        Some(_) => format!("{} AppImage", hya_updater::appimage_arch()),
        None => format!("{}-{}", hya_updater::os_tag(), hya_updater::arch_tag()),
    }
}

/// Whether a package manager owns this binary — a deb or rpm in `/usr/bin`,
/// a `.pkg` in `/Applications`. A tarball or Homebrew install is not one,
/// even though `dpkg` exists on the same machine.
fn package_managed() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        .is_some_and(|dir| !hya_updater::update_method(&dir).is_self_update())
}
