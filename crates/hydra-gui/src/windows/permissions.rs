// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The permissions guide: what access Hydra needs on this OS, whether it
//! currently has it, and one-click jumps into the right settings pane.
//!
//! Nothing here can GRANT anything — that is the operating system's job by
//! design — but the guide turns "os error 13" mysteries into a checklist.

use crate::app::{App, El, Message, WinKind};
use crate::windows::{dlg_btn, dlg_btn_auto, dlg_btn_primary};
use crate::{i18n::tr, theme};
use iced::widget::{column, container, row, scrollable, text};
use iced::Length;

/// Live probe results shown as status dots.
#[derive(Clone, Copy, Debug, Default)]
pub struct PermStatus {
    /// Write access to the default download folder.
    pub folder: Option<bool>,
    /// Full Disk Access (macOS only; probed via a TCC-protected path).
    pub full_disk: Option<bool>,
    /// Login item registered (when "Launch on startup" is on).
    pub login_item: Option<bool>,
}

/// Cheap synchronous probes; called when the window opens and on Tick while
/// it stays open.
pub fn probe(download_dir: &str, autostart_expected: bool) -> PermStatus {
    let probe_file = std::path::Path::new(download_dir).join(".hydra-access-probe");
    let folder = std::fs::create_dir_all(download_dir)
        .and_then(|()| std::fs::write(&probe_file, b"ok"))
        .map(|()| {
            let _ = std::fs::remove_file(&probe_file);
        })
        .is_ok();

    #[cfg(target_os = "macos")]
    let full_disk = dirs::home_dir().map(|h| {
        // Readable only with Full Disk Access.
        std::fs::File::open(h.join("Library/Application Support/com.apple.TCC/TCC.db")).is_ok()
    });
    #[cfg(not(target_os = "macos"))]
    let full_disk = None;

    #[cfg(target_os = "macos")]
    let login_item = autostart_expected.then(crate::autostart::is_registered);
    #[cfg(not(target_os = "macos"))]
    let login_item = {
        let _ = autostart_expected;
        None
    };

    PermStatus {
        folder: Some(folder),
        full_disk,
        login_item,
    }
}

fn status_dot<'a>(ok: Option<bool>) -> El<'a> {
    let (glyph, color) = match ok {
        Some(true) => ("●", iced::Color::from_rgb8(0x2E, 0x7D, 0x32)),
        Some(false) => ("●", iced::Color::from_rgb8(0xC0, 0x2B, 0x2B)),
        None => ("○", theme::dim_text(&iced::Theme::Light)),
    };
    text(glyph).size(theme::FONT_SIZE + 2.0).color(color).into()
}

/// One access item: status dot and title on the left, the settings-pane
/// button on the same line at the right, the explanation under them.
fn entry<'a>(
    status: Option<bool>,
    title: String,
    body: String,
    action: Option<(String, Message)>,
) -> El<'a> {
    let mut head = row![
        status_dot(status),
        text(title).size(theme::FONT_SIZE + 1.0),
        iced::widget::space::horizontal(),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);
    if let Some((label, msg)) = action {
        // Label-sized: "Open Files & Folders settings" wrapped to two lines
        // in the fixed-width dialog button.
        head = head.push(dlg_btn_auto(label, Some(msg)));
    }
    container(
        column![
            head,
            text(body)
                .size(theme::FONT_SIZE)
                .color(theme::dim_text(&iced::Theme::Light)),
        ]
        .spacing(6),
    )
    .padding(10)
    .width(Length::Fill)
    .style(theme::panel)
    .into()
}

pub fn view(app: &App) -> El<'_> {
    let st = app.perm_status;
    let mut items = column![].spacing(10).width(Length::Fill);

    // Download folder access — every OS.
    items = items.push(entry(
        st.folder,
        tr("Download folder access"),
        tr("Hydra must write into your download folders. macOS asks the first time; if it was denied, allow Hydra under Files & Folders — or pick the folder once through the system dialog, which grants it automatically."),
        if cfg!(target_os = "macos") {
            Some((
                tr("Open Files & Folders settings"),
                Message::PermOpenPane(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders",
                ),
            ))
        } else {
            None
        },
    ));

    if cfg!(target_os = "macos") {
        items = items.push(entry(
            st.full_disk,
            tr("Full Disk Access (optional)"),
            tr("Lets Hydra save into any folder without per-folder prompts. Add Hydra with the + button, switch it on, then relaunch the app. Not required for normal use."),
            Some((
                tr("Open Full Disk Access settings"),
                Message::PermOpenPane(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles",
                ),
            )),
        ));
        items = items.push(entry(
            st.login_item,
            tr("Login item"),
            tr("\"Launch Hydra on startup\" registers a login item; macOS lists it under General > Login Items where you can verify or disable it."),
            Some((
                tr("Open Login Items settings"),
                Message::PermOpenPane(
                    "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
                ),
            )),
        ));
    }

    if cfg!(target_os = "windows") {
        items = items.push(entry(
            st.folder,
            tr("Controlled folder access"),
            tr("If Windows Security's ransomware protection blocks Hydra from Documents/Downloads, allow Hydra under \"Allow an app through Controlled folder access\"."),
            Some((
                tr("Open Windows Security"),
                Message::PermOpenPane("windowsdefender://ransomware"),
            )),
        ));
    }

    if cfg!(target_os = "linux") {
        items = items.push(entry(
            None,
            tr("Sandboxed packaging"),
            tr("When packaged as Flatpak or Snap, file access goes through portals: pick folders through the file dialog once and the portal remembers the grant."),
            None,
        ));
    }

    // No in-window heading: the OS title bar already says "Permissions".
    // Refresh sits with OK in the button row, where IDM keeps every
    // dialog-level action.
    container(
        column![
            text(tr("Green means Hydra verified the access just now; red means the OS is currently refusing it."))
                .size(theme::FONT_SIZE - 1.0)
                .color(theme::dim_text(&iced::Theme::Light)),
            scrollable(items).height(Length::Fill),
            row![
                iced::widget::space::horizontal(),
                dlg_btn(tr("Refresh"), Some(Message::PermRefresh)),
                dlg_btn_primary(tr("OK"), app.win_of(WinKind::Permissions).map(Message::CloseThis)),
            ]
            .spacing(10),
        ]
        .spacing(10)
        .padding(14),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}
