// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Configuration window: General, File types, Save to, Downloads,
//! Connection, Proxy/Socks, Sites Logins, Extensions, Sounds — two-row
//! tab strip.

use crate::app::{
    upsert_exception, upsert_login, upsert_profile, App, El, Message, OptField, OptTab,
    OptionsState, WinKind,
};
use crate::model::{ProxyMode, ProxyType, SiteLogin};
use crate::sounds;
use crate::windows::{cell, check, dlg_btn, dlg_btn_auto, dlg_btn_auto_primary, dlg_btn_primary};
use crate::{i18n::tr, theme};
use iced::widget::{
    button, column, container, pick_list, radio, row, scrollable, text, text_editor, text_input,
    tooltip,
};
use iced::Length;

fn o(f: OptField) -> Message {
    Message::OptDraft(f)
}

fn tab_btn<'a>(label: String, tab: OptTab, cur: OptTab) -> El<'a> {
    button(crate::windows::centered(label, theme::FONT_SIZE))
        .padding([4, 10])
        .width(Length::Fill)
        .style(theme::btn_tab(tab == cur))
        .on_press(Message::OptTabSet(tab))
        .into()
}

/// Wrap a control with a hover hint.
fn hinted<'a>(el: impl Into<El<'a>>, hint: String) -> El<'a> {
    tooltip(
        el,
        container(text(hint).size(theme::FONT_SIZE - 1.0))
            .padding(8)
            .max_width(360.0)
            .style(theme::menu_panel),
        tooltip::Position::Bottom,
    )
    .into()
}

/// Palette entries are packed hex; unpack one for a `text` colour.
fn hex(v: u32) -> iced::Color {
    iced::Color::from_rgb8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// A number box that has to hold at least 1 while `required`: blank or
/// zero would be stored as a cap that blocks everything.
fn number_style(
    typed: &str,
    required: bool,
) -> fn(&iced::Theme, iced::widget::text_input::Status) -> iced::widget::text_input::Style {
    if !required || typed.trim().parse::<u64>().is_ok_and(|n| n > 0) {
        theme::input
    } else {
        theme::input_invalid
    }
}

fn section<'a>(title: String) -> El<'a> {
    row![text(title).size(theme::FONT_SIZE + 2.0),]
        .width(Length::Fill)
        .into()
}

fn startup_checkbox<'a>(enabled: bool, portable: bool) -> iced::widget::Checkbox<'a, Message> {
    check(enabled && !portable, tr("Launch Hydra on startup"))
        .on_toggle_maybe((!portable).then_some(|b| o(OptField::LaunchStartup(b))))
}

fn startup_controls<'a>(enabled: bool, portable: bool) -> El<'a> {
    let mut startup = column![hinted(
        startup_checkbox(enabled, portable),
        tr("Registers Hydra as a login item so downloads and queues continue after a reboot."),
    )]
    .spacing(4);
    if portable {
        startup = startup.push(
            text(tr("Launch on startup is unavailable for portable profiles to preserve the installed copy's startup entry."))
                .size(theme::FONT_SIZE - 1.0),
        );
    }
    startup.into()
}

fn general(app: &App) -> El<'_> {
    let s = &app.options.draft;
    // Which extensions are talking to Hydra right now. A tick with nothing
    // beside it means the box is set but no extension has connected — which
    // is the difference between "capture is off" and "capture cannot happen",
    // and the list gave no way to tell them apart before.
    let live = crate::extbus::live_browsers();
    let mut browsers = column![].spacing(4);
    for (i, (name, on)) in s.capture_browsers.iter().enumerate() {
        let connected = live.iter().any(|b| b.eq_ignore_ascii_case(name));
        browsers = browsers.push(
            row![
                container(check(*on, name.clone()).on_toggle(move |b| o(OptField::Browser(i, b))),)
                    .width(Length::Fill),
                text(if connected {
                    tr("extension connected")
                } else {
                    String::new()
                })
                .size(theme::FONT_SIZE - 1.0)
                .color(hex(theme::PROGRESS_GREEN)),
            ]
            .align_y(iced::Alignment::Center),
        );
    }
    // Dock/taskbar visibility exists only where the tray can take over the
    // app while no window is open.
    #[cfg(target_os = "macos")]
    let hide_taskbar: Option<El<'_>> = Some(hinted(
        check(s.hide_from_taskbar, tr("Hide Dock icon")).on_toggle(|b| o(OptField::HideTaskbar(b))),
        tr("Removes Hydra from the Dock and Cmd-Tab while it runs in the tray; the Dock icon and menu bar return while a window is open."),
    ));
    #[cfg(target_os = "windows")]
    let hide_taskbar: Option<El<'_>> = Some(hinted(
        check(s.hide_from_taskbar, tr("Hide from taskbar")).on_toggle(|b| o(OptField::HideTaskbar(b))),
        tr("Hydra windows get no taskbar button; reach the app from the tray icon. Applies to windows opened after the change."),
    ));
    // Linux: an X11 window-manager hint per window. Wayland has no
    // skip-taskbar protocol at all — a checkbox that cannot act would only
    // look broken, so it exists on X11 sessions only. (winit picks Wayland
    // exactly when WAYLAND_DISPLAY is set, so that is the session test.)
    #[cfg(target_os = "linux")]
    let hide_taskbar: Option<El<'_>> = std::env::var_os("WAYLAND_DISPLAY")
        .is_none()
        .then(|| {
            hinted(
                check(s.hide_from_taskbar, tr("Hide from taskbar")).on_toggle(|b| o(OptField::HideTaskbar(b))),
                tr("Keeps Hydra windows out of the taskbar and the workspace switcher; reach the app from the tray icon. Wayland has no way to hide an open window, so there it applies while Hydra runs in the tray."),
            )
        });
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let hide_taskbar: Option<El<'_>> = None;
    let mut col =
        column![
        section(tr("Browser/System Integration")),
        startup_controls(s.launch_on_startup, crate::model::app_dir_override().is_some()),
        hinted(
            check(s.start_in_tray, tr("Launch minimized to system tray")).on_toggle(|b| o(OptField::StartInTray(b))),
            tr("Autostart launches stay in the tray; open the window from the tray icon."),
        ),
        hinted(
            check(s.close_to_tray, tr("Close to system tray")).on_toggle(|b| o(OptField::CloseToTray(b))),
            tr("Closing the main window leaves Hydra running in the tray, where queues and transfers carry on; open it again from the tray icon. Off: closing the window exits Hydra."),
        ),
    ];
    // Straight after "Close to system tray": both decide what the app looks
    // like once its window is gone.
    if let Some(el) = hide_taskbar {
        col = col.push(el);
    }
    col.extend([
        hinted(
            check(s.check_updates_on_startup, tr("Check for updates on startup")).on_toggle(|b| o(OptField::CheckUpdates(b))),
            tr("Asks the release server for a newer Hydra when the app starts. Only the check is automatic; installing always waits for your confirmation."),
        ),
        hinted(
            check(s.beta_channel, tr("Download Beta channel")).on_toggle(|b| o(OptField::BetaChannel(b))),
            tr("Update checks also offer release candidates (-rc tags) when one is ahead of the stable release; otherwise the stable release is used. Beta builds may be less stable."),
        ),
        hinted(
            check(s.power_save, tr("Power save mode")).on_toggle(|b| o(OptField::PowerSave(b))),
            tr("Fewer wakeups: slower interface refresh, no progress animation, coarser transfer ticks. Download speed is unchanged."),
        ),
        hinted(
            check(s.gpu_render, tr("Use GPU render for smoother interface")).on_toggle(|b| o(OptField::GpuRender(b))),
            tr("GPU rendering is smoother on very large windows but uses considerably more memory and the graphics processor. Takes effect after restart."),
        ),
        hinted(
            check(s.monitor_clipboard, tr("Automatically start downloading of URLs placed to clipboard")).on_toggle(|b| o(OptField::Clipboard(b))),
            tr("Watches the clipboard for download links by file type and known download sites; one link opens the file dialog, many open the batch list."),
        ),
        text(tr("Capture downloads from the following browsers:"))
            .size(theme::FONT_SIZE)
            .into(),
        container(browsers)
            .padding(10)
            .width(Length::Fill)
            .style(theme::panel)
            .into(),
        text(tr(
            "Hydra registers itself with these browsers automatically; install the Hydra extension in each one you tick."
        ))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light))
            .into(),
    ])
    .spacing(10)
    .into()
}

fn file_types(app: &App) -> El<'_> {
    let s = &app.options.draft;
    let _ = s;
    column![
        section(tr("Downloaded file types")),
        text(tr(
            "Automatically start downloading the following file types:"
        ))
        .size(theme::FONT_SIZE),
        text_editor(&app.options.auto_types_edit)
            .on_action(|a| o(OptField::AutoTypesEdit(a)))
            .size(theme::FONT_SIZE)
            .height(90.0),
        text(tr(
            "Don't start downloading automatically from the following sites:"
        ))
        .size(theme::FONT_SIZE),
        text_editor(&app.options.sites_edit)
            .on_action(|a| o(OptField::SitesEdit(a)))
            .size(theme::FONT_SIZE)
            .height(70.0),
        text(tr("(separate with commas or spaces)"))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
    ]
    .spacing(10)
    .into()
}

fn save_to(app: &App) -> El<'_> {
    let st = &app.options;
    let cats: Vec<String> = st.draft_cats.iter().map(|c| c.name.clone()).collect();
    let cur = st
        .draft_cats
        .iter()
        .find(|c| c.name == st.sel_category)
        .cloned()
        .unwrap_or_else(|| st.draft_cats[0].clone());
    // General is the catch-all: its list is "whatever nothing else claimed",
    // so it is described rather than edited. Rename and Remove answer to a
    // different question — the stock categories keep their names.
    let editable = st.cat_types_editable();
    let free_name = st.can_name_category();
    let types: El<'_> = if editable {
        column![
            text_editor(&st.cat_exts_edit)
                .placeholder("PNG JPG WEBP")
                .on_action(|a| o(OptField::CatExtsEdit(a)))
                .size(theme::FONT_SIZE)
                .height(90.0),
            text(tr("(separate names by spaces)"))
                .size(theme::FONT_SIZE - 1.0)
                .color(theme::dim_text(&iced::Theme::Light)),
        ]
        .spacing(4)
        .into()
    } else {
        container(
            text(tr(
                "The file types that are not listed in any other category",
            ))
            .size(theme::FONT_SIZE),
        )
        .padding(8)
        .width(Length::Fill)
        .style(theme::panel)
        .into()
    };
    column![
        section(tr("Categories, file types, folders")),
        text(tr("Category")).size(theme::FONT_SIZE),
        pick_list(cats, Some(st.sel_category.clone()), |c| o(OptField::SelCategory(c)))
            .text_size(theme::FONT_SIZE)
            .style(theme::picker)
            .width(300.0),
        row![
            text_input(&format!("{}: .ext .ext", tr("Category name")), &st.cat_name)
                .on_input(|v| o(OptField::CatName(v)))
                .on_submit(o(OptField::CatAdd))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            dlg_btn_auto(tr("New"), free_name.then(|| o(OptField::CatAdd))),
            dlg_btn_auto(
                tr("Rename"),
                st.can_rename_category().then(|| o(OptField::CatRename))
            ),
            dlg_btn_auto(
                tr("Remove"),
                st.cat_is_removable().then(|| o(OptField::CatRemove))
            ),
        ]
        .spacing(8),
        text(format!(
            "{} \"{}\" {}:",
            tr("Automatically put in"),
            st.sel_category,
            tr("category the following file types")
        ))
        .size(theme::FONT_SIZE),
        types,
        text(format!(
            "{} \"{}\" {}",
            tr("Default download directory for"),
            st.sel_category,
            tr("category")
        ))
        .size(theme::FONT_SIZE),
        row![
            text_input("", &cur.dir)
                .on_input(|v| o(OptField::CatDir(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            dlg_btn(tr("Browse"), Some(o(OptField::BrowseCatDir))),
        ]
        .spacing(8),
        hinted(
            check(app.options.draft.no_category_dirs, tr("Do not create category folders — save everything in the default folder")).on_toggle(|b| o(OptField::NoCatDirs(b))),
            format!(
                "{}\n{}",
                tr("Off (default): a download is filed in its category folder, e.g. Downloads/Video."),
                tr("On: the folders above are ignored and new downloads are saved directly in the General category folder. Downloads already on the list keep their folder."),
            ),
        ),
        check(app.options.draft.remember_last_dir, format!(
                "{} \"{}\" {}",
                tr("Change folder for"),
                app.options.sel_category,
                tr("category on last selected")
            )).on_toggle(|b| o(OptField::RememberLast(b))),
        check(app.options.draft.server_file_date, tr("Set file creation date as provided by the server")).on_toggle(|b| o(OptField::ServerDate(b))),
        text(tr("File parts are stored next to the destination as \"<name>.part\" and renamed in place on completion — no temporary directory is needed."))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
    ]
    .spacing(8)
    .into()
}

fn downloads(app: &App) -> El<'_> {
    let s = &app.options.draft;
    column![
        section(tr("Customize \"Download progress\" dialog")),
        hinted(
            check(s.start_minimized, tr("Start download progress dialog minimized")).on_toggle(|b| o(OptField::StartMinimized(b))),
            tr("New progress windows open minimized to the Dock/taskbar instead of in front."),
        ),
        hinted(
            check(s.show_file_info_dialog, tr("Show \"Download File Info\" dialog before starting")).on_toggle(|b| o(OptField::ShowFileInfo(b))),
            tr("Adding a link first shows name/category/folder while the transfer already runs in the background; off = downloads start immediately."),
        ),
        hinted(
            check(s.bg_download, tr("Download in background while choosing options")).on_toggle(|b| o(OptField::BgDownload(b))),
            tr("Off = nothing is fetched until \"Start Download\" is pressed, so a rename or a change of folder happens before the transfer, not during it."),
        ),
        hinted(
            check(s.show_speed_tab, tr("Show \"Speed Limiter\" tab")).on_toggle(|b| o(OptField::SpeedTab(b))),
            tr("Shows or hides the Speed Limiter tab of the progress window."),
        ),
        hinted(
            check(s.show_completion_tab, tr("Show \"Options on completion\" tab")).on_toggle(|b| o(OptField::CompletionTab(b))),
            tr("Shows or hides the Options-on-completion tab of the progress window."),
        ),
        hinted(
            check(s.show_hide_buttons, tr("Show \"Hide tab\" buttons")).on_toggle(|b| o(OptField::HideButtons(b))),
            tr("Shows a Hide-tab button inside the Speed Limiter and Options-on-completion tabs."),
        ),
        hinted(
            check(s.show_conn_details, tr("Show connection details")).on_toggle(|b| o(OptField::ConnDetails(b))),
            tr("Off = the progress window opens collapsed; its \"Show details\" button still opens the per-connection panel."),
        ),
        hinted(
            check(s.show_complete_dialog, tr("Show download complete dialog")).on_toggle(|b| o(OptField::CompleteDialog(b))),
            tr("Pops the completion dialog with Open / Open folder when a download finishes."),
        ),
        hinted(
            check(s.remove_completed, tr("Remove completed downloads from the list")).on_toggle(|b| o(OptField::RemoveCompleted(b))),
            tr("A finished download drops off the list on its own — once the complete dialog is closed, when that dialog is shown. The downloaded file is kept."),
        ),
        section(tr("Virus checking")),
        text(tr("Virus scanner program")).size(theme::FONT_SIZE),
        row![
            text_input("", &s.virus_scanner)
                .on_input(|v| o(OptField::VirusScanner(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            dlg_btn(tr("Browse"), Some(o(OptField::BrowseVirus))),
        ]
        .spacing(8),
        text(tr("Command line parameters")).size(theme::FONT_SIZE),
        text_input("", &s.virus_args)
            .on_input(|v| o(OptField::VirusArgs(v)))
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill),
        text(tr("User-Agent for manually added downloads:")).size(theme::FONT_SIZE),
        pick_list(UA_PRESETS, ua_preset(&s.user_agent), |p| o(OptField::UserAgent(
            p.value.to_string()
        )))
        .placeholder(tr("Custom"))
        .text_size(theme::FONT_SIZE)
        .style(theme::picker)
        .padding([5, 8])
        .width(Length::Fill),
        text_input("", &s.user_agent)
            .on_input(|v| o(OptField::UserAgent(v)))
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill),
    ]
    .spacing(8)
    .into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UaPreset {
    label: &'static str,
    value: &'static str,
}

impl std::fmt::Display for UaPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label)
    }
}

const UA_PRESETS: &[UaPreset] = &[
    UaPreset {
        label: "Default (IE 11)",
        value: crate::model::DEFAULT_USER_AGENT,
    },
    UaPreset {
        label: "Chrome",
        value: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36",
    },
    UaPreset {
        label: "Firefox",
        value: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:156.0) Gecko/20100101 Firefox/156.0",
    },
    UaPreset {
        label: "Safari",
        value: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
                (KHTML, like Gecko) Version/27.0 Safari/605.1.15",
    },
    UaPreset {
        label: "Edge",
        value: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36 Edg/153.0.0.0",
    },
];

/// The preset the agent box currently holds, or `None` — shown as Custom —
/// once it has been edited into anything else.
fn ua_preset(agent: &str) -> Option<UaPreset> {
    UA_PRESETS.iter().copied().find(|p| p.value == agent)
}

/// One-line readout of the live window beneath the limit controls: what the
/// running limit has counted and when it rolls over. Reads the *saved*
/// settings, not the draft — the draft is not in force until OK is pressed.
fn quota_line(app: &App) -> String {
    let s = &app.cfg.settings;
    if !s.dl_limit_enabled {
        return tr("Limit off: transfers are not counted.");
    }
    let q = &app.state.dl_quota;
    if q.window_start == 0 {
        return tr("The period starts with the next downloaded byte.");
    }
    let left = crate::app::quota_window_secs(s)
        .saturating_sub(crate::fmt::now_unix().saturating_sub(q.window_start))
        .max(0) as u64;
    format!(
        "{} {} / {} \u{2014} {} {}",
        tr("Used this period:"),
        crate::fmt::size2(q.used),
        crate::fmt::size2(crate::app::quota_cap(s).unwrap_or(0)),
        tr("resets in"),
        crate::fmt::eta(left),
    )
}

/// The picker's entry for "do not read cookies from a browser".
///
/// Public because `App::update` compares against it: the picker speaks labels
/// and the setting stores a `BROWSER[:PROFILE]` string, so exactly one value
/// has to mean "off" and both sides must agree which.
pub const COOKIES_OFF: &str = "Do not";

/// Every browser the picker offers, taken from the library rather than listed
/// here so a browser added there shows up without a second edit.
fn cookie_browsers() -> Vec<String> {
    std::iter::once(tr(COOKIES_OFF))
        .chain(
            hya_net::cookies::browser::Browser::ALL
                .iter()
                .map(|b| b.name().to_string()),
        )
        .collect()
}

fn cookie_browser(s: &crate::model::Settings) -> String {
    match s.cookies_from_browser.split(':').next().unwrap_or("") {
        "" => tr(COOKIES_OFF),
        b => b.to_string(),
    }
}

fn cookie_profile(s: &crate::model::Settings) -> &str {
    s.cookies_from_browser
        .split_once(':')
        .map(|(_, p)| p)
        .unwrap_or("")
}

/// The setting after the browser picker moved to `label`, keeping whatever
/// profile was already named.
///
/// Both halves of the setting are edited by separate controls but stored as one
/// `BROWSER[:PROFILE]` string, so each edit has to rebuild the whole thing. A
/// pure function rather than two arms of the options handler because getting
/// `chrome:Profile 2` back out of "the user just changed the browser" is the
/// part that can be wrong, and it is testable on its own.
pub fn with_browser(current: &str, label: &str) -> String {
    if label == tr(COOKIES_OFF) || label.is_empty() {
        return String::new();
    }
    match current.split_once(':') {
        Some((_, profile)) if !profile.trim().is_empty() => format!("{label}:{profile}"),
        _ => label.to_string(),
    }
}

/// The setting after the profile box was edited. With no browser chosen there
/// is nothing for a profile to be a profile OF, so it stays empty.
pub fn with_profile(current: &str, profile: &str) -> String {
    let browser = current.split(':').next().unwrap_or("");
    match (browser.is_empty(), profile.trim().is_empty()) {
        (true, _) => String::new(),
        (false, true) => browser.to_string(),
        (false, false) => format!("{browser}:{}", profile.trim()),
    }
}

/// Field edits that touch nothing outside the dialog's own state. The pickers
/// and the cookie-store check need the app and stay in
/// [`App::on_opt_field`](crate::app::App); their results come back here as
/// `*Picked` fields.
impl OptionsState {
    pub fn apply(&mut self, f: OptField) {
        match f {
            OptField::AutoTypesEdit(a) => {
                self.auto_types_edit.perform(a);
                self.draft.auto_types = self.auto_types_edit.text();
                return;
            }
            OptField::SitesEdit(a) => {
                self.sites_edit.perform(a);
                self.draft.dont_start_sites = self.sites_edit.text();
                return;
            }
            OptField::CatExtsEdit(a) => {
                self.cat_exts_edit.perform(a);
                return;
            }
            // The Download-limit numbers keep a text buffer beside the draft:
            // digits only, and the draft takes the value only when it parses,
            // so a momentarily empty field is a legal editing state instead of
            // an ignored keystroke.
            OptField::DlLimitMb(v) => {
                let v: String = v.chars().filter(|c| c.is_ascii_digit()).take(9).collect();
                if let Ok(n) = v.parse() {
                    self.draft.dl_limit_mb = n;
                }
                self.dl_limit_mb_txt = v;
                return;
            }
            OptField::DlLimitHours(v) => {
                let v: String = v.chars().filter(|c| c.is_ascii_digit()).take(5).collect();
                if let Ok(n) = v.parse() {
                    self.draft.dl_limit_hours = n;
                }
                self.dl_limit_hours_txt = v;
                return;
            }
            OptField::SpeedLimitKb(v) => {
                let v: String = v.chars().filter(|c| c.is_ascii_digit()).take(9).collect();
                // Blank or zero is "no number yet", not a cap of zero — a
                // zero cap would stall every transfer under it.
                self.draft.global_speed_limit = v
                    .parse::<u64>()
                    .ok()
                    .filter(|kb| *kb > 0)
                    .map(|kb| kb * 1024);
                self.speed_limit_kb_txt = v;
                return;
            }
            _ => {}
        }
        let s = &mut self.draft;
        match f {
            OptField::LaunchStartup(b) => s.launch_on_startup = b,
            OptField::CheckUpdates(b) => s.check_updates_on_startup = b,
            OptField::BetaChannel(b) => s.beta_channel = b,
            OptField::StartInTray(b) => s.start_in_tray = b,
            OptField::CloseToTray(b) => s.close_to_tray = b,
            #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
            OptField::HideTaskbar(b) => s.hide_from_taskbar = b,
            OptField::PowerSave(b) => s.power_save = b,
            OptField::GpuRender(b) => s.gpu_render = b,
            OptField::Clipboard(b) => s.monitor_clipboard = b,
            OptField::PortableCapture(b) => s.portable_capture = b,
            OptField::Untrust(i) => {
                if i < s.allowed_extensions.len() {
                    s.allowed_extensions.remove(i);
                }
            }
            OptField::Browser(i, b) => {
                if let Some(x) = s.capture_browsers.get_mut(i) {
                    x.1 = b;
                }
            }
            OptField::AutoTypesEdit(_)
            | OptField::SitesEdit(_)
            | OptField::CatExtsEdit(_)
            | OptField::DlLimitMb(_)
            | OptField::DlLimitHours(_)
            | OptField::SpeedLimitKb(_) => unreachable!(),
            OptField::FfmpegPath(path) | OptField::FfmpegPicked(Some(path)) => {
                self.ffmpeg_path_valid =
                    hya_stream::hls::valid_ffmpeg_path(std::path::Path::new(&path));
                s.ffmpeg_path = path;
            }
            OptField::FfmpegPicked(None) | OptField::BrowseFfmpeg => {}
            OptField::BrowseVirus | OptField::BrowseCatDir | OptField::SoundBrowse(_) => {}
            OptField::RememberLast(b) => s.remember_last_dir = b,
            OptField::ServerDate(b) => s.server_file_date = b,
            OptField::NoCatDirs(b) => s.no_category_dirs = b,
            OptField::ShowFileInfo(b) => s.show_file_info_dialog = b,
            OptField::BgDownload(b) => s.bg_download = b,
            OptField::StartMinimized(b) => s.start_minimized = b,
            OptField::SpeedTab(b) => s.show_speed_tab = b,
            OptField::CompletionTab(b) => s.show_completion_tab = b,
            OptField::HideButtons(b) => s.show_hide_buttons = b,
            OptField::ConnDetails(b) => s.show_conn_details = b,
            OptField::CompleteDialog(b) => s.show_complete_dialog = b,
            OptField::RemoveCompleted(b) => s.remove_completed = b,
            OptField::UserAgent(v) => s.user_agent = v,
            OptField::VirusScanner(v) => s.virus_scanner = v,
            OptField::VirusArgs(v) => s.virus_args = v,
            OptField::VirusPicked(Some(p)) => s.virus_scanner = p,
            OptField::VirusPicked(None) => {}
            OptField::DefaultConns(n) => s.default_conns = n,
            OptField::AdaptiveConns(b) => s.adaptive_conns = b,
            // Stored as the one `BROWSER[:PROFILE]` string the CLI parses, so
            // the two surfaces cannot disagree about what a profile is.
            OptField::CookiesBrowser(name) => {
                s.cookies_from_browser = with_browser(&s.cookies_from_browser, &name);
            }
            OptField::CookiesProfile(v) => {
                s.cookies_from_browser = with_profile(&s.cookies_from_browser, &v);
            }
            OptField::ExcSel(i) => {
                self.sel_exc = Some(i);
                if let Some((server, n)) = self.draft.conn_exceptions.get(i) {
                    self.conn_exc_server = server.clone();
                    self.conn_exc_n = n.to_string();
                }
            }
            OptField::ExcServer(v) => self.conn_exc_server = v,
            OptField::ExcConns(v) => self.conn_exc_n = v,
            OptField::ExcAdd => {
                let server = self.conn_exc_server.trim().to_string();
                let n: usize = self.conn_exc_n.trim().parse().unwrap_or(0);
                if !server.is_empty() && n > 0 {
                    upsert_exception(
                        &mut self.draft.conn_exceptions,
                        server,
                        n.clamp(1, crate::model::MAX_CONNECTIONS),
                    );
                    self.sel_exc = None;
                    self.conn_exc_server.clear();
                    self.conn_exc_n.clear();
                }
            }
            OptField::ExcRemove => {
                if let Some(i) = self.sel_exc.take() {
                    if i < self.draft.conn_exceptions.len() {
                        self.draft.conn_exceptions.remove(i);
                    }
                    self.conn_exc_server.clear();
                    self.conn_exc_n.clear();
                }
            }
            OptField::DlLimit(b) => s.dl_limit_enabled = b,
            OptField::SpeedLimiter(b) => s.speed_limiter_on = b,
            OptField::ProfileSel(i) => {
                self.sel_profile = Some(i);
                if let Some(p) = self.draft.speed_profiles.get(i) {
                    self.profile_name = p.name.clone();
                    self.profile_kb = p.limit.map(|b| (b / 1024).to_string()).unwrap_or_default();
                }
            }
            OptField::ProfileName(v) => self.profile_name = v,
            OptField::ProfileKb(v) => {
                self.profile_kb = v.chars().filter(|c| c.is_ascii_digit()).take(9).collect()
            }
            OptField::ProfileAdd => {
                let name = self.profile_name.trim().to_string();
                if !name.is_empty() {
                    // Blank speed makes an unlimited profile — the one that
                    // clears the cap, which every profile list needs.
                    let limit = self
                        .profile_kb
                        .trim()
                        .parse::<u64>()
                        .ok()
                        .filter(|kb| *kb > 0)
                        .map(|kb| kb * 1024);
                    upsert_profile(&mut self.draft.speed_profiles, name, limit);
                    self.sel_profile = None;
                    self.profile_name.clear();
                    self.profile_kb.clear();
                }
            }
            OptField::ProfileRemove => {
                if let Some(i) = self.sel_profile.take() {
                    if i < self.draft.speed_profiles.len() {
                        self.draft.speed_profiles.remove(i);
                    }
                    self.profile_name.clear();
                    self.profile_kb.clear();
                }
            }
            OptField::WarnStop(b) => s.warn_before_stop = b,
            OptField::ProxyMode(m) => s.proxy_mode = m,
            OptField::ProxyScript(v) => s.proxy_script = v,
            OptField::ProxyHost(v) => s.proxy_host = v,
            OptField::ProxyPort(v) => s.proxy_port = v,
            OptField::ProxyUser(v) => s.proxy_user = v,
            OptField::ProxyPass(v) => s.proxy_pass = v,
            OptField::ProxyType(t) => s.proxy_type = t,
            OptField::SelCategory(c) => self.select_category(c),
            OptField::CatName(v) => self.cat_name = v,
            OptField::CatAdd => self.add_category(),
            OptField::CatRename => self.rename_category(),
            OptField::CatRemove => self.remove_category(),
            OptField::CatDir(v) | OptField::CatDirPicked(Some(v)) => {
                let sel = self.sel_category.clone();
                if let Some(c) = self.draft_cats.iter_mut().find(|c| c.name == sel) {
                    c.dir = v;
                }
            }
            OptField::CatDirPicked(None) => {}
            OptField::LoginSel(i) => {
                self.sel_login = Some(i);
                if let Some(l) = self.draft.logins.get(i) {
                    self.login_site = l.site.clone();
                    self.login_user = l.user.clone();
                    self.login_pass = l.pass.clone();
                }
            }
            OptField::LoginSite(v) => self.login_site = v,
            OptField::LoginUser(v) => self.login_user = v,
            OptField::LoginPass(v) => self.login_pass = v,
            OptField::LoginAdd => {
                let site = self.login_site.trim().to_string();
                if !site.is_empty() {
                    let login = SiteLogin {
                        site,
                        user: self.login_user.clone(),
                        pass: self.login_pass.clone(),
                    };
                    upsert_login(&mut self.draft.logins, login);
                    self.sel_login = None;
                    self.login_site.clear();
                    self.login_user.clear();
                    self.login_pass.clear();
                }
            }
            OptField::LoginRemove => {
                if let Some(i) = self.sel_login.take() {
                    if i < self.draft.logins.len() {
                        self.draft.logins.remove(i);
                    }
                }
            }
            OptField::Sound(i, b) => {
                if let Some(row) = s.sounds.get_mut(i) {
                    row.enabled = b;
                }
            }
            OptField::SoundPicked(i, p) => {
                if let Some(row) = s.sounds.get_mut(i) {
                    row.file = p;
                }
            }
            OptField::SoundPlay(i) => {
                if let Some(row) = s.sounds.get(i) {
                    sounds::play(
                        (!row.file.is_empty()).then(|| row.file.clone()),
                        sounds::Event::from_index(i),
                    );
                }
            }
        }
    }
}

/// What the chosen browser's store turned out to be, or why it could not be
/// read.
///
/// Answered when the browser is PICKED, not when a download needs it: the
/// common failure is a permission the user has to grant somewhere else
/// entirely, and discovering that one download at a time is discovering it in
/// the wrong place.
fn cookie_status(st: &crate::app::OptionsState) -> crate::app::El<'_> {
    if st.cookie_checking {
        return indented(
            text(tr("Checking..."))
                .size(theme::FONT_SIZE - 1.0)
                .color(theme::dim_text(&iced::Theme::Light)),
        );
    }
    match &st.cookie_check {
        Some(Ok(path)) => indented(
            text(format!("{} {path}", tr("Will read:")))
                .size(theme::FONT_SIZE - 1.0)
                .color(theme::dim_text(&iced::Theme::Light)),
        ),
        Some(Err(why)) => indented(
            text(why.clone())
                .size(theme::FONT_SIZE - 1.0)
                .color(iced::Color::from_rgb8(0xC0, 0x2B, 0x2B)),
        ),
        None => iced::widget::space::horizontal().height(0.0).into(),
    }
}

/// A line under the row above it, lined up with that row's fields.
fn indented<'a>(t: iced::widget::Text<'a, iced::Theme, iced::Renderer>) -> crate::app::El<'a> {
    row![
        iced::widget::space::horizontal().width(COOKIE_LABEL_W),
        t.width(Length::Fill),
    ]
    .into()
}

/// Width of the "Use cookies from" label, so the line under it lines up with
/// the picker rather than with the window edge.
const COOKIE_LABEL_W: f32 = 110.0;

/// Height of the Connection tab's two row lists.
///
/// The tab is long enough that its height is a budget, not a free choice: at
/// the stock font the whole page has to fit the Configuration window without
/// the body scrolling. This is what is left for each list once the rest of
/// the page has taken its share — three rows plus the 3pt inset, which shows
/// the stock speed profiles in full and still hints at a fourth row when a
/// list has one.
const LIST_H: f32 = 70.0;
/// Floor for the PAC address label.
const PAC_LABEL_W: f32 = 80.0;
/// The two fixed columns of the saved-logins list, header and rows off the
/// same numbers.
const LOGIN_USER_W: f32 = 140.0;
const LOGIN_PASS_W: f32 = 120.0;

/// A selectable row in one of the Connection lists.
///
/// The exception list and the speed-profile list are the same shape — a
/// left-aligned name, a right-aligned value, selected or not — and live on
/// different sub-tabs, so the shape is a function rather than written twice.
fn list_row<'a>(
    name: String,
    value: String,
    value_w: f32,
    selected: bool,
    on_press: Message,
) -> El<'a> {
    button(row![cell(name, Length::Fill), cell(value, value_w)].spacing(6))
        .padding([1, 2])
        .width(Length::Fill)
        .style(theme::btn_row(selected))
        .on_press(on_press)
        .into()
}

fn conn_limits(app: &App) -> El<'_> {
    let s = &app.options.draft;
    let st = &app.options;
    let conn_opts = crate::model::CONNECTION_OPTIONS.as_slice();
    let mut exc = column![].spacing(2);
    for (i, (server, n)) in s.conn_exceptions.iter().enumerate() {
        exc = exc.push(list_row(
            server.clone(),
            n.to_string(),
            70.0,
            st.sel_exc == Some(i),
            o(OptField::ExcSel(i)),
        ));
    }
    // The Number box takes 1..=256, the range every connection count is
    // clamped to; New waits until it holds one.
    let exc_n_ok = st
        .conn_exc_n
        .trim()
        .parse::<usize>()
        .is_ok_and(|n| (1..=crate::model::MAX_CONNECTIONS).contains(&n));
    column![
        section(tr("Connections and Limits")),
        row![
            text(tr("Default max. conn. number")).size(theme::FONT_SIZE),
            pick_list(conn_opts, Some(s.default_conns), |n| o(
                OptField::DefaultConns(n)
            ))
            .text_size(theme::FONT_SIZE)
            .style(theme::picker)
            .width(90.0),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
        hinted(
            check(s.adaptive_conns, tr("Measure and adapt connection count")).on_toggle(|b| o(OptField::AdaptiveConns(b))),
            tr("Starts each transfer with one connection and adds more only while they measurably improve speed; the default max. number acts as a ceiling."),
        ),
        text(tr("Exceptions:")).size(theme::FONT_SIZE),
        container(crate::ui::scroll(exc).height(Length::Fill))
            .padding(3)
            .width(Length::Fill)
            .height(LIST_H)
            .style(theme::panel),
        row![
            text_input(&tr("Server"), &st.conn_exc_server)
                .on_input(|v| o(OptField::ExcServer(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            text_input(&tr("Number"), &st.conn_exc_n)
                .on_input(|v| o(OptField::ExcConns(v)))
                .size(theme::FONT_SIZE)
                .style(if exc_n_ok || st.conn_exc_n.trim().is_empty() {
                    theme::input
                } else {
                    theme::input_invalid
                })
                .width(90.0),
            dlg_btn(
                tr("New"),
                (exc_n_ok && !st.conn_exc_server.trim().is_empty())
                    .then(|| o(OptField::ExcAdd)),
            ),
            dlg_btn(
                tr("Remove"),
                st.sel_exc.map(|_| o(OptField::ExcRemove)),
            ),
        ]
        .spacing(8),
    ]
    .spacing(8)
    .into()
}

fn conn_cookies(app: &App) -> El<'_> {
    let s = &app.options.draft;
    let st = &app.options;
    column![
        section(tr("Cookies")),
        hinted(
            row![
                text(tr("Use cookies from")).size(theme::FONT_SIZE),
                pick_list(cookie_browsers(), Some(cookie_browser(s)), |b| o(
                    OptField::CookiesBrowser(b)
                ))
                .text_size(theme::FONT_SIZE)
                .style(theme::picker)
                .padding([5, 8])
                .width(150.0),
                text(tr("Profile")).size(theme::FONT_SIZE),
                text_input(&tr("default"), cookie_profile(s))
                    .on_input(|v| o(OptField::CookiesProfile(v)))
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(Length::Fill),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            tr(
                "Reads that browser's own cookie store for downloads you add by hand, \
                and only for the site being downloaded from. Downloads captured by the \
                Hydra browser extension already carry the page's cookies and are not \
                affected. The Add URL dialog names the file it read."
            ),
        ),
        cookie_status(st),
    ]
    .spacing(8)
    .into()
}

fn conn_speed(app: &App) -> El<'_> {
    let s = &app.options.draft;
    let st = &app.options;
    let mut profiles = column![].spacing(2);
    for (i, p) in s.speed_profiles.iter().enumerate() {
        profiles = profiles.push(list_row(
            tr(&p.name),
            crate::fmt::limit(p.limit),
            90.0,
            st.sel_profile == Some(i),
            o(OptField::ProfileSel(i)),
        ));
    }
    column![
        section(tr("Speed limiter")),
        // Switch and value share a row. Two rows read no better and this tab
        // has to fit its window without scrolling.
        row![
            hinted(
                check(s.speed_limiter_on, tr("Limit download speed")).on_toggle(|b| o(OptField::SpeedLimiter(b))),
                tr("Caps the combined speed of every download that has no limit of its own. The toolbar's Speed Limit button switches the same cap on and off while downloads run."),
            ),
            text_input("500", &st.speed_limit_kb_txt)
                .on_input(|v| o(OptField::SpeedLimitKb(v)))
                .size(theme::FONT_SIZE)
                .style(number_style(&st.speed_limit_kb_txt, s.speed_limiter_on))
                .width(80.0),
            text(tr("KB/sec")).size(theme::FONT_SIZE),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        // The hint rides the label, not the row below it: a tooltip over the
        // name and speed boxes would pop up while they are being typed into.
        hinted(
            text(tr("Profiles:")).size(theme::FONT_SIZE),
            tr("Named caps the toolbar's Speed Limit arrow switches between in one click. Leave the speed empty for a profile that turns the limiter off; saving a name that is already in the list retunes it."),
        ),
        container(crate::ui::scroll(profiles).height(Length::Fill))
            .padding(3)
            .width(Length::Fill)
            .height(LIST_H)
            .style(theme::panel),
        row![
            text_input(&tr("Name"), &st.profile_name)
                .on_input(|v| o(OptField::ProfileName(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            text_input(&tr("KB/sec"), &st.profile_kb)
                .on_input(|v| o(OptField::ProfileKb(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(90.0),
            dlg_btn(tr("New"), Some(o(OptField::ProfileAdd))),
            dlg_btn(
                tr("Remove"),
                st.sel_profile.map(|_| o(OptField::ProfileRemove)),
            ),
        ]
        .spacing(8),
    ]
    .spacing(8)
    .into()
}

fn conn_quota(app: &App) -> El<'_> {
    let s = &app.options.draft;
    let st = &app.options;
    column![
        section(tr("Download limits")),
        hinted(
            check(s.dl_limit_enabled, tr("Download limits")).on_toggle(|b| o(OptField::DlLimit(b))),
            tr("Caps how much Hydra may transfer per period — for metered or capped connections. Transfers pause when the cap is reached and resume by themselves when the next period starts."),
        ),
        row![
            text(tr("Download no more than")).size(theme::FONT_SIZE),
            text_input("200", &st.dl_limit_mb_txt)
                .on_input(|v| o(OptField::DlLimitMb(v)))
                .size(theme::FONT_SIZE)
                .style(number_style(&st.dl_limit_mb_txt, s.dl_limit_enabled))
                .width(80.0),
            text(tr("MBytes every")).size(theme::FONT_SIZE),
            text_input("5", &st.dl_limit_hours_txt)
                .on_input(|v| o(OptField::DlLimitHours(v)))
                .size(theme::FONT_SIZE)
                .style(number_style(&st.dl_limit_hours_txt, s.dl_limit_enabled))
                .width(60.0),
            text(tr("hours")).size(theme::FONT_SIZE),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        text(quota_line(app))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
        check(s.warn_before_stop, tr("Show warning before stopping downloads")).on_toggle(|b| o(OptField::WarnStop(b))),
    ]
    .spacing(8)
    .into()
}

fn proxy(app: &App) -> El<'_> {
    let s = &app.options.draft;
    let mode = s.proxy_mode;
    // Manual fields that describe no proxy: said under the row while they
    // are typed, and OK refuses them — stored, they would silently connect
    // directly.
    let manual_note: El<'_> = match (mode, crate::proxy::manual_problem(s)) {
        (ProxyMode::Manual, Some(why)) => text(why)
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::error_text())
            .into(),
        _ => iced::widget::space::horizontal().height(0.0).into(),
    };
    column![
        section(tr("Proxy / socks configuration")),
        radio(tr("No proxy/socks"), ProxyMode::None, Some(mode), |m| o(
            OptField::ProxyMode(m)
        ))
        .size(15.0)
        .text_size(theme::FONT_SIZE),
        hinted(
            radio(
                tr("Use system settings"),
                ProxyMode::System,
                Some(mode),
                |m| { o(OptField::ProxyMode(m)) }
            )
            .size(15.0)
            .text_size(theme::FONT_SIZE),
            tr(
                "Takes the proxy from the environment (all_proxy, https_proxy, http_proxy) \
                 and then from the system settings. A SOCKS proxy is preferred where the \
                 system offers both."
            ),
        ),
        radio(
            tr("Use automatic configuration script"),
            ProxyMode::Script,
            Some(mode),
            |m| o(OptField::ProxyMode(m)),
        )
        .size(15.0)
        .text_size(theme::FONT_SIZE),
        row![
            text(tr("Address"))
                .size(theme::FONT_SIZE)
                .wrapping(iced::widget::text::Wrapping::None)
                .width(crate::windows::label_width(&tr("Address"), PAC_LABEL_W)),
            text_input("", &s.proxy_script)
                .on_input(|v| o(OptField::ProxyScript(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
        ]
        .spacing(8),
        // Said here rather than only in the log, and in red once the radio
        // is on: OK refuses it, because a route that quietly does nothing is
        // how a download ends up leaving through the real address while the
        // user believes it is tunnelled.
        text(tr(crate::proxy::PAC_UNSUPPORTED))
            .size(theme::FONT_SIZE - 1.0)
            .color(if mode == ProxyMode::Script {
                theme::error_text()
            } else {
                theme::dim_text(&iced::Theme::Light)
            }),
        radio(
            tr("Manual proxy/socks configuration"),
            ProxyMode::Manual,
            Some(mode),
            |m| o(OptField::ProxyMode(m)),
        )
        .size(15.0)
        .text_size(theme::FONT_SIZE),
        row![
            column![
                text(tr("Type"))
                    .size(theme::FONT_SIZE)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                pick_list(&ProxyType::ALL[..], Some(s.proxy_type), |t| o(
                    OptField::ProxyType(t)
                ))
                .text_size(theme::FONT_SIZE)
                .style(theme::picker)
                .width(Length::Fill),
            ]
            .spacing(4)
            .width(110.0),
            column![
                text(tr("Proxy server address"))
                    .size(theme::FONT_SIZE)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                hinted(
                    text_input("", &s.proxy_host)
                        .on_input(|v| o(OptField::ProxyHost(v)))
                        .size(theme::FONT_SIZE)
                        .style(theme::input),
                    tr("A host name or address, or a full specification such as \
                         socks5://127.0.0.1:10808 — a scheme spelled here wins over Type."),
                ),
            ]
            .spacing(4)
            .width(Length::Fill),
            column![
                text(tr("Port"))
                    .size(theme::FONT_SIZE)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                text_input("", &s.proxy_port)
                    .on_input(|v| o(OptField::ProxyPort(v)))
                    .size(theme::FONT_SIZE)
                    .style(theme::input),
            ]
            .spacing(4)
            .width(90.0),
            column![
                text(tr("UserName"))
                    .size(theme::FONT_SIZE)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                text_input("", &s.proxy_user)
                    .on_input(|v| o(OptField::ProxyUser(v)))
                    .size(theme::FONT_SIZE)
                    .style(theme::input),
            ]
            .spacing(4)
            .width(140.0),
            column![
                text(tr("Password"))
                    .size(theme::FONT_SIZE)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                text_input("", &s.proxy_pass)
                    .on_input(|v| o(OptField::ProxyPass(v)))
                    .secure(true)
                    .size(theme::FONT_SIZE)
                    .style(theme::input),
            ]
            .spacing(4)
            .width(140.0),
        ]
        .spacing(10),
        manual_note,
        text(tr(
            "The proxy carries every download: HTTP, HTTPS and — over SOCKS — FTP."
        ))
        .size(theme::FONT_SIZE - 1.0)
        .color(theme::dim_text(&iced::Theme::Light)),
    ]
    .spacing(8)
    .into()
}

fn sites(app: &App) -> El<'_> {
    let st = &app.options;
    let mut list = column![].spacing(2);
    list = list.push(
        row![
            cell(tr("Site/path"), Length::Fill),
            cell(tr("User"), LOGIN_USER_W),
            cell(tr("Password"), LOGIN_PASS_W),
        ]
        .spacing(6),
    );
    for (i, l) in st.draft.logins.iter().enumerate() {
        let selected = st.sel_login == Some(i);
        list = list.push(
            button(
                row![
                    cell(l.site.clone(), Length::Fill),
                    cell(l.user.clone(), LOGIN_USER_W),
                    cell("•••".to_string(), LOGIN_PASS_W),
                ]
                .spacing(6),
            )
            .padding([1, 2])
            .width(Length::Fill)
            .style(theme::btn_row(selected))
            .on_press(o(OptField::LoginSel(i))),
        );
    }
    column![
        section(tr("User names and passwords for servers/sites")),
        container(crate::ui::scroll(list).height(220.0))
            .padding(6)
            .width(Length::Fill)
            .style(theme::panel),
        row![
            text_input(&tr("Site/path"), &st.login_site)
                .on_input(|v| o(OptField::LoginSite(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            text_input(&tr("User"), &st.login_user)
                .on_input(|v| o(OptField::LoginUser(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(130.0),
            text_input(&tr("Password"), &st.login_pass)
                .on_input(|v| o(OptField::LoginPass(v)))
                .secure(true)
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(130.0),
        ]
        .spacing(8),
        row![
            dlg_btn(
                tr("New"),
                (!st.login_site.trim().is_empty()).then(|| o(OptField::LoginAdd)),
            ),
            dlg_btn(tr("Remove"), st.sel_login.map(|_| o(OptField::LoginRemove)),),
        ]
        .spacing(10),
    ]
    .spacing(10)
    .into()
}

/// The Chrome Web Store listing. Edge has a store of its own (below); the
/// remaining Chromium browsers install the same item from here.
const CHROME_STORE: &str =
    "https://chromewebstore.google.com/detail/hydra-download-manager-in/oieelfilllghmbnhofajpgpmmilfihmo";

/// The Firefox Add-ons listing.
/// The setup guide behind the FFmpeg row. A wiki page rather than a
/// paragraph in this dialog: what to install differs per platform, and it
/// changes faster than the app ships.
const FFMPEG_WIKI: &str = "https://github.com/ja7ad/hydra/wiki/Hydra-ffmpeg-integration";

/// The "not present" counterpart to `theme::PROGRESS_GREEN`.
const OFFLINE_RED: u32 = 0xB3462E;

const FIREFOX_STORE: &str = "https://addons.mozilla.org/en-US/firefox/addon/hdm-integration/";

/// The Microsoft Edge Add-ons listing: same extension, Edge's own store.
const EDGE_STORE: &str =
    "https://microsoftedge.microsoft.com/addons/detail/hydra-download-manager-in/obemipfpeenmhkdpkobdkeedhdakaoai";

/// One extension row: brand mark on the left, what the extension does in the
/// middle, the link button on the right. `link` is `None` for a browser with
/// neither a listing nor a guide, which leaves the button disabled.
fn ext_row<'a>(
    icon: iced::widget::svg::Handle,
    name: &'static str,
    about: String,
    label: String,
    link: Option<&'static str>,
) -> El<'a> {
    let msg = link.map(Message::OptExtStore);
    container(
        row![
            iced::widget::svg(icon).width(30.0).height(30.0),
            column![
                text(name).size(theme::FONT_SIZE + 1.0),
                text(about)
                    .size(theme::FONT_SIZE - 1.0)
                    // A translated description can hold one word longer than
                    // the row is wide, and word wrapping alone lets it run
                    // out past the panel edge.
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .color(theme::dim_text(&iced::Theme::Light)),
            ]
            .spacing(3)
            .width(Length::Fill),
            // Auto width, not the fixed dialog-button width: store labels
            // run long once translated and a fixed 132 px clips them.
            match msg {
                Some(m) => dlg_btn_auto_primary(label, Some(m)),
                None => dlg_btn_auto(label, None),
            },
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center),
    )
    .padding(10)
    .width(Length::Fill)
    .style(theme::panel)
    .into()
}

/// Trim a filesystem path down to one readable line: the root, an ellipsis,
/// and the tail that actually identifies the binary.
///
/// Windows package managers bury ffmpeg absurdly deep: winget's copy sits
/// under `%LOCALAPPDATA%\Microsoft\WinGet\Packages`, behind a package folder
/// and a versioned build folder, for 120-odd characters that no dialog row
/// can hold. The head and the last few components say where it came from
/// and what it is; the full string is one hover away.
fn elide_path(path: &std::path::Path, budget: usize) -> String {
    let full = path.display().to_string();
    if full.chars().count() <= budget {
        return full;
    }
    let sep = if full.contains('\\') { '\\' } else { '/' };
    let parts: Vec<&str> = full.split(sep).collect();
    // The head is the drive on Windows (`C:`) and the empty string before
    // the leading slash on Unix, which is exactly what makes `/…/bin/ffmpeg`
    // come out right.
    let head = parts.first().copied().unwrap_or_default();
    let mut tail: Vec<&str> = Vec::new();
    // The head, plus the separator on either side of the ellipsis.
    let mut used = head.chars().count() + 3;
    for part in parts.iter().skip(1).rev() {
        let cost = part.chars().count() + 1;
        // The file name goes in whatever it costs: a row that elides down to
        // "C:\…" has told the reader nothing at all.
        if !tail.is_empty() && used + cost > budget {
            break;
        }
        used += cost;
        tail.push(part);
    }
    tail.reverse();
    format!("{head}{sep}\u{2026}{sep}{}", tail.join(&sep.to_string()))
}

/// A status word beside the FFmpeg title: the dot carries the same meaning,
/// and a dot alone is no answer for anyone who cannot tell the two colours
/// apart.
fn status_badge<'a>(label: String, colour: iced::Color) -> El<'a> {
    row![
        text("\u{25CF}").size(theme::FONT_SIZE - 3.0).color(colour),
        text(label).size(theme::FONT_SIZE - 1.0).color(colour),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center)
    .into()
}

/// What the FFmpeg row has to say, worked out before anything is laid out.
///
/// A built row is a tree of shapes, and none of the questions worth asking
/// about this one survive being turned into one: is the guide offered only
/// while it has something to tell you? is a path shown only when there is
/// one? So the answers are decided here, and the widgets below merely draw
/// them.
struct FfmpegRow {
    found: bool,
    badge: String,
    about: String,
    /// The path as the row shows it and as the clipboard gets it: elided for
    /// the eye, whole for the paste.
    path: Option<(String, String)>,
    action: String,
    /// The setup guide, offered only when it can still help.
    guide: Option<&'static str>,
}

impl FfmpegRow {
    fn of(found: Option<&std::path::Path>) -> Self {
        match found {
            Some(path) => Self {
                found: true,
                badge: tr("Installed"),
                about: tr(
                    "MPEG-TS is remuxed to MP4, and DASH video and audio are merged into one file.",
                ),
                path: Some((elide_path(path, 58), path.display().to_string())),
                action: tr("Copy path"),
                guide: None,
            },
            None => Self {
                found: false,
                badge: tr("Not found"),
                about: tr("Not found on PATH. HLS and DASH still download, but MPEG-TS is saved as .ts instead of .mp4, and DASH audio stays in its own file."),
                path: None,
                action: tr("FFmpeg setup guide"),
                guide: Some(FFMPEG_WIKI),
            },
        }
    }
}

/// The FFmpeg row, which reports what is actually on THIS machine rather
/// than describing ffmpeg in the abstract: a green badge and the path it was
/// found at, or a red one and the guide.
///
/// The path is a line of its own rather than a tail glued to the status
/// sentence. Glued on, a Windows path wraps as one unbreakable word, runs
/// out past the panel and straight under the button — and it buried the part
/// that matters (which ffmpeg is this?) inside a paragraph.
fn ffmpeg_row<'a>(st: FfmpegRow) -> El<'a> {
    let colour = if st.found {
        hex(theme::PROGRESS_GREEN)
    } else {
        hex(OFFLINE_RED)
    };
    let actions = column![
        dlg_btn(tr("Browse"), Some(o(OptField::BrowseFfmpeg))),
        dlg_btn(
            tr("Reset Path"),
            Some(o(OptField::FfmpegPath(String::new())))
        ),
        dlg_btn(
            tr("Copy path"),
            st.path
                .as_ref()
                .map(|(_, full)| Message::OptCopy(full.clone()))
        ),
    ]
    .spacing(6);
    let mut info = column![
        row![
            text("FFmpeg").size(theme::FONT_SIZE + 1.0),
            status_badge(st.badge, colour)
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        text(st.about)
            .size(theme::FONT_SIZE - 1.0)
            .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
            .color(theme::dim_text(&iced::Theme::Light)),
    ]
    .spacing(3)
    .width(Length::Fill);
    if let Some((shown, full)) = st.path {
        info = info.push(tooltip(
            text(shown)
                .size(theme::FONT_SIZE - 1.0)
                .wrapping(iced::widget::text::Wrapping::None)
                .color(theme::dim_text(&iced::Theme::Light)),
            container(
                text(full)
                    .size(theme::FONT_SIZE - 1.0)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            )
            .padding(8)
            .max_width(420.0)
            .style(theme::menu_panel),
            tooltip::Position::Top,
        ));
    }
    if let Some(url) = st.guide {
        info = info.push(dlg_btn_auto_primary(
            st.action,
            Some(Message::OptExtStore(url)),
        ));
    }
    container(
        row![
            iced::widget::svg(crate::icons::folder_video())
                .width(30.0)
                .height(30.0),
            info,
            actions,
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center),
    )
    .padding(10)
    .width(Length::Fill)
    .style(theme::panel)
    .into()
}

fn extensions(app: &App) -> El<'_> {
    // Only a `--config DIR` copy has the choice to make: without the flag
    // this instance IS the one the browsers are registered against, and a
    // switch that can only be on is not a switch.
    let portable: Option<El<'_>> = crate::model::app_dir_override().map(|dir| {
        column![
            section(tr("Portable copy")),
            hinted(
                check(app.options.draft.portable_capture, tr("Let this copy handle browser capture")).on_toggle(|b| o(OptField::PortableCapture(b))),
                tr("Registers this copy's helper with your browsers so capture reaches this profile, and lets a browser start Hydra on it when nothing is running. Browser registration is per user, so this takes capture away from any ordinary Hydra install on this account."),
            ),
            text(format!("{} {}", tr("Profile:"), dir.display()))
                .size(theme::FONT_SIZE - 1.0)
                .color(theme::dim_text(&iced::Theme::Light)),
        ]
        .spacing(6)
        .into()
    });
    // Which browsers can start Hydra from the extension: the native-host
    // manifests this launch registered. A connected extension with no
    // manifest works only while Hydra is already running.
    let registered = crate::nmhost::registered();
    let host_line = if registered.is_empty() {
        tr("Native host registered with: none yet")
    } else {
        format!(
            "{} {}",
            tr("Native host registered with:"),
            registered.join(", ")
        )
    };
    let mut col = column![
        section(tr("Browser extensions")),
        text(tr(
            "Install the Hydra extension to capture downloads straight from your browser."
        ))
        .size(theme::FONT_SIZE),
        text(host_line)
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
        ext_row(
            crate::icons::browser_chrome(),
            "Google Chrome",
            tr("Automatic download capture, right-click downloads and media sniffing. Published on the Chrome Web Store."),
            tr("Open Chrome Web Store"),
            Some(CHROME_STORE),
        ),
        ext_row(
            crate::icons::browser_firefox(),
            "Mozilla Firefox",
            tr("Automatic download capture, right-click downloads and media sniffing. Published on the Firefox Addons."),
            tr("Open Firefox Addons"),
            Some(FIREFOX_STORE),
        ),
        ext_row(
            crate::icons::browser_edge(),
            "Microsoft Edge",
            tr("Automatic download capture, right-click downloads and media sniffing. Published on Microsoft Edge Add-ons."),
            tr("Open Edge Add-ons"),
            Some(EDGE_STORE),
        ),
        ext_row(
            crate::icons::browser_chromium(),
            "Brave / Vivaldi / Opera / Arc / Chromium",
            tr("Chromium browsers install the very same extension from the Chrome Web Store."),
            tr("Open Chrome Web Store"),
            Some(CHROME_STORE),
        ),
        ext_row(
            crate::icons::browser_safari(),
            "Safari",
            tr("Ships inside the macOS app; enable Hydra under Safari > Settings > Extensions."),
            tr("Not on store yet"),
            None,
        ),
    ]
    .spacing(10);
    let allowed = &app.options.draft.allowed_extensions;
    if !allowed.is_empty() {
        let mut list = column![
            section(tr("Trusted extensions")),
            text(tr("Allowed to connect without Hydra's native host."))
                .size(theme::FONT_SIZE - 1.0)
                .color(theme::dim_text(&iced::Theme::Light)),
        ]
        .spacing(6);
        for (i, origin) in allowed.iter().enumerate() {
            list = list.push(
                row![
                    text(origin.clone())
                        .size(theme::FONT_SIZE)
                        .width(Length::Fill),
                    dlg_btn_auto(tr("Remove"), Some(o(OptField::Untrust(i)))),
                ]
                .align_y(iced::Alignment::Center),
            );
        }
        col = col.push(list);
    }
    if let Some(el) = portable {
        col = col.push(el);
    }
    col.into()
}

fn ffmpeg_state(options: &OptionsState) -> FfmpegRow {
    let selected = &options.draft.ffmpeg_path;
    let mut state = if selected.is_empty() {
        FfmpegRow::of(hya_stream::hls::automatic_ffmpeg().as_deref())
    } else {
        FfmpegRow::of(
            options
                .ffmpeg_path_valid
                .then(|| std::path::Path::new(selected)),
        )
    };
    if !selected.is_empty() {
        state.about =
            tr("MPEG-TS is remuxed to MP4, and DASH video and audio are merged into one file.");
        state.path = Some((
            elide_path(std::path::Path::new(selected), 58),
            selected.clone(),
        ));
    }
    state
}

/// The external tools a download may need after the bytes arrive.
///
/// Its own page rather than a section under the browser extensions: ffmpeg is
/// what remuxes MPEG-TS and merges DASH video with its audio, which has
/// nothing to do with which browser captured the download.
fn media_tools(app: &App) -> El<'_> {
    let state = ffmpeg_state(&app.options);
    column![section(tr("Media tools")), ffmpeg_row(state)]
        .spacing(10)
        .into()
}

/// The file column, header and rows off the same number.
const SOUND_FILE_W: f32 = 230.0;

fn sounds(app: &App) -> El<'_> {
    let s = &app.options.draft;
    let mut list = column![].spacing(4);
    list = list.push(
        row![
            cell(tr("Event"), Length::Fill),
            cell(tr("Sound file"), SOUND_FILE_W),
        ]
        .spacing(6),
    );
    for (i, snd) in s.sounds.iter().enumerate() {
        let file_label = if snd.file.is_empty() {
            tr("(default chime)")
        } else {
            snd.file.clone()
        };
        list = list.push(
            row![
                check(snd.enabled, tr(&snd.event))
                    .on_toggle(move |b| o(OptField::Sound(i, b)))
                    .width(Length::Fill),
                cell(file_label, SOUND_FILE_W),
                // Sized to their labels: every row carries the same two, so
                // the grid still lines up, and the event column keeps the
                // ~130px a uniform button would have taken from it.
                dlg_btn_auto(tr("Browse"), Some(o(OptField::SoundBrowse(i)))),
                dlg_btn_auto(tr("Play"), Some(o(OptField::SoundPlay(i)))),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        );
    }
    column![
        section(tr("Sound settings")),
        text(tr("Select sounds for download events")).size(theme::FONT_SIZE),
        text(tr("Supported formats: .wav and .ogg. A built-in chime plays when no file is set or the file is missing."))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
        container(list).padding(10).width(Length::Fill).style(theme::panel),
    ]
    .spacing(10)
    .into()
}

/// The top-level groups, in order, each holding the pages it owns.
///
/// Nine tabs over two rows became four over one plus a row of sub-tabs: the
/// chrome is the same height, and the hierarchy is stated instead of implied
/// by a flat list where "Sounds" sat beside "Connection". It is also where a
/// page contributed by a plugin goes, without a tenth top-level tab and a
/// third row to hold it.
///
/// Every [`OptTab`] must appear exactly once; [`GROUPS`] is the only place
/// that decides where a page lives, and a test holds it to both halves of
/// that.
const GROUPS: &[(&str, &[OptTab])] = &[
    // "Application", not "General": the group holds the General page, and a
    // group named after one of its own pages reads as a tab containing itself.
    ("Application", &[OptTab::General, OptTab::Sounds]),
    (
        "Files",
        &[OptTab::FileTypes, OptTab::SaveTo, OptTab::Downloads],
    ),
    (
        "Connection",
        &[
            OptTab::Connection,
            OptTab::Cookies,
            OptTab::SpeedLimit,
            OptTab::Quota,
            OptTab::Proxy,
            OptTab::Sites,
        ],
    ),
    (
        "Extensions & Plugins",
        &[OptTab::Extensions, OptTab::MediaTools, OptTab::Plugins],
    ),
];

/// The sub-tab label for one page.
///
/// `Connection` reads "Connections" because it is no longer the whole tab —
/// it is the connection-count page inside the group that carries that name.
fn leaf_label(t: OptTab) -> &'static str {
    match t {
        OptTab::General => "General",
        OptTab::Sounds => "Sounds",
        OptTab::FileTypes => "File types",
        OptTab::SaveTo => "Save to",
        OptTab::Downloads => "Downloads",
        OptTab::Connection => "Connections",
        OptTab::Cookies => "Cookies",
        OptTab::SpeedLimit => "Speed limiter",
        OptTab::Quota => "Download limits",
        OptTab::Proxy => "Proxy / Socks",
        OptTab::Sites => "Sites Logins",
        OptTab::Extensions => "Browser extensions",
        OptTab::MediaTools => "Media tools",
        OptTab::Plugins => "Plugins",
    }
}

/// The group holding `cur`, falling back to the first so a page that is
/// somehow unlisted still draws a window rather than none.
fn group_of(cur: OptTab) -> &'static (&'static str, &'static [OptTab]) {
    GROUPS
        .iter()
        .find(|(_, pages)| pages.contains(&cur))
        .unwrap_or(&GROUPS[0])
}

/// A sub-tab: lighter than [`tab_btn`], so the second row reads as belonging
/// to the group above it rather than as another bank of tabs.
fn sub_btn<'a>(label: String, page: OptTab, cur: OptTab) -> El<'a> {
    button(crate::windows::centered(label, theme::FONT_SIZE - 1.0))
        .padding([3, 12])
        .style(theme::btn_tab(page == cur))
        .on_press(Message::OptTabSet(page))
        .into()
}

pub fn view(app: &App) -> El<'_> {
    let cur = app.options.tab;
    let (_, pages) = group_of(cur);

    let mut groups = row![].spacing(1);
    for (name, members) in GROUPS {
        // A group already open keeps the page the user is on; otherwise
        // selecting it lands on its first. No second message type: the group
        // button IS a page button, it just picks which page.
        let target = if members.contains(&cur) {
            cur
        } else {
            members[0]
        };
        groups = groups.push(tab_btn(tr(name), target, cur));
    }
    // A group with one page draws no sub-tab row: a single button that is
    // always selected and goes nowhere is a row of chrome saying nothing. The
    // pane takes the height instead.
    let subs: El<'_> = if pages.len() > 1 {
        let mut r = row![].spacing(6);
        for page in *pages {
            r = r.push(sub_btn(tr(leaf_label(*page)), *page, cur));
        }
        r.into()
    } else {
        iced::widget::space::vertical().height(0.0).into()
    };

    let body: El<'_> = match cur {
        OptTab::General => general(app),
        OptTab::FileTypes => file_types(app),
        OptTab::SaveTo => save_to(app),
        OptTab::Downloads => downloads(app),
        OptTab::Connection => conn_limits(app),
        OptTab::Cookies => conn_cookies(app),
        OptTab::SpeedLimit => conn_speed(app),
        OptTab::Quota => conn_quota(app),
        OptTab::Proxy => proxy(app),
        OptTab::Sites => sites(app),
        OptTab::Extensions => extensions(app),
        OptTab::MediaTools => media_tools(app),
        OptTab::Plugins => crate::plugins::view(app),
        OptTab::Sounds => sounds(app),
    };

    // The notebook metaphor the two-row bar needed a row swap for now holds by
    // construction: the sub-tab row is always the one adjacent to the pane.
    container(
        column![
            groups,
            subs,
            container(scrollable(container(body).padding(14).width(Length::Fill)))
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::panel),
            row![
                match &app.options.error {
                    Some(why) => text(why.clone())
                        .size(theme::FONT_SIZE - 1.0)
                        .color(theme::error_text()),
                    None => text(""),
                },
                iced::widget::space::horizontal(),
                dlg_btn_primary(tr("OK"), Some(Message::OptOk)),
                dlg_btn(
                    tr("Cancel"),
                    app.win_of(WinKind::Options).map(Message::CloseThis)
                ),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(6)
        .padding(10),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn browsing_updates_the_ffmpeg_subtitle_and_badge_before_saving() {
        let mut options = OptionsState::default();
        let executable = std::env::current_exe().unwrap().display().to_string();
        options.apply(OptField::FfmpegPicked(Some(executable.clone())));
        let selected = ffmpeg_state(&options);
        assert!(selected.found);
        assert_eq!(selected.badge, tr("Installed"));
        assert_eq!(selected.path.unwrap().1, executable);
        options.apply(OptField::FfmpegPicked(Some(
            "/missing-hydra-ffmpeg/executable".into(),
        )));
        let missing = ffmpeg_state(&options);
        assert!(!missing.found);
        assert_eq!(missing.badge, tr("Not found"));
        assert_eq!(missing.path.unwrap().1, "/missing-hydra-ffmpeg/executable");
        options.apply(OptField::FfmpegPath(String::new()));
        let reset = ffmpeg_state(&options);
        assert_eq!(reset.found, hya_stream::hls::automatic_ffmpeg().is_some());
    }

    #[test]
    fn ffmpeg_picker_cancel_preserves_path_and_reset_clears_it() {
        let mut state = crate::app::OptionsState::default();
        state.apply(OptField::FfmpegPicked(Some("F:/Tools/ffmpeg.exe".into())));
        state.apply(OptField::FfmpegPicked(None));
        assert_eq!(state.draft.ffmpeg_path, "F:/Tools/ffmpeg.exe");
        state.apply(OptField::FfmpegPath(String::new()));
        assert!(state.draft.ffmpeg_path.is_empty());
    }

    #[test]
    fn startup_checkbox_only_toggles_for_the_default_profile() {
        use iced::advanced::{layout, widget, Layout, Shell, Widget};
        use iced::{mouse, Event, Point, Rectangle, Size};

        let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(theme::FONT_SIZE),
        ));
        let node = layout::Node::new(Size::new(300.0, 20.0));
        for portable in [false, true] {
            for enabled in [false, true] {
                let controls = startup_controls(enabled, portable);
                let tree = widget::Tree::new(controls.as_widget());
                assert_eq!(tree.children.len(), if portable { 2 } else { 1 });
                let mut checkbox = startup_checkbox(enabled, portable);
                let mut tree = widget::Tree::empty();
                let mut messages = Vec::new();
                checkbox.update(
                    &mut tree,
                    &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Layout::new(&node),
                    mouse::Cursor::Available(Point::new(5.0, 5.0)),
                    &renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &Rectangle::with_size(Size::new(300.0, 20.0)),
                );
                if portable {
                    assert!(messages.is_empty());
                } else {
                    assert!(matches!(
                        messages.as_slice(),
                        [Message::OptDraft(OptField::LaunchStartup(value))] if *value == !enabled
                    ));
                }
            }
        }
    }

    #[test]
    fn connection_limit_view_accepts_the_full_range() {
        assert_eq!(
            crate::model::CONNECTION_OPTIONS,
            [1, 2, 4, 8, 16, 32, 64, 128, 256]
        );
        let mut app = App::default();
        for typed in ["", "0", "1", "64", "128", "256", "257", "invalid"] {
            app.options.conn_exc_server = "example.com".into();
            app.options.conn_exc_n = typed.into();
            let _ = conn_limits(&app);
        }
    }

    #[test]
    fn connection_settings_keep_all_choices_and_large_server_exceptions() {
        let mut st = OptionsState::default();
        for n in crate::model::CONNECTION_OPTIONS {
            st.apply(OptField::DefaultConns(n));
            st.apply(OptField::ExcServer("example.com".into()));
            st.apply(OptField::ExcConns(n.to_string()));
            st.apply(OptField::ExcAdd);
            assert_eq!(st.draft.default_conns, n);
            assert_eq!(st.draft.conn_exceptions, vec![("example.com".into(), n)]);
        }
        st.apply(OptField::ExcServer("example.org".into()));
        st.apply(OptField::ExcConns("257".into()));
        st.apply(OptField::ExcAdd);
        assert_eq!(st.draft.conn_exceptions[1].1, 256);
    }

    /// An extension installed from a listing this page links to must be one
    /// the native host allow-lists and the WebSocket trusts; a mismatch left
    /// every Web Store install unable to reach the app (#262).
    #[test]
    fn every_chromium_store_listing_is_an_allowed_extension() {
        for url in [CHROME_STORE, EDGE_STORE] {
            let id = url.rsplit('/').next().unwrap();
            assert!(
                crate::nmhost::CHROMIUM_EXT_IDS.contains(&id),
                "{id} from {url} is not in nmhost::CHROMIUM_EXT_IDS"
            );
        }
    }

    /// Every page belongs to exactly one group. A page listed twice would draw
    /// under whichever group `group_of` found first; a page listed nowhere
    /// would be unreachable, and `view` would silently fall back to General
    /// while the menu deep-link still pointed at it.
    #[test]
    fn every_page_is_in_exactly_one_group() {
        const ALL: &[OptTab] = &[
            OptTab::General,
            OptTab::FileTypes,
            OptTab::SaveTo,
            OptTab::Downloads,
            OptTab::Connection,
            OptTab::Cookies,
            OptTab::SpeedLimit,
            OptTab::Quota,
            OptTab::Proxy,
            OptTab::Sites,
            OptTab::Extensions,
            OptTab::MediaTools,
            OptTab::Sounds,
            OptTab::Plugins,
        ];
        for page in ALL {
            let holding: Vec<&str> = GROUPS
                .iter()
                .filter(|(_, pages)| pages.contains(page))
                .map(|(name, _)| *name)
                .collect();
            assert_eq!(holding.len(), 1, "{page:?} is in {holding:?}");
        }
        let listed: usize = GROUPS.iter().map(|(_, pages)| pages.len()).sum();
        assert_eq!(
            listed,
            ALL.len(),
            "GROUPS and ALL disagree on the page list"
        );
    }

    #[test]
    fn a_group_is_found_from_any_page_it_holds() {
        assert_eq!(group_of(OptTab::Sounds).0, "Application");
        assert_eq!(group_of(OptTab::SaveTo).0, "Files");
        assert_eq!(group_of(OptTab::Proxy).0, "Connection");
        assert_eq!(group_of(OptTab::Quota).0, "Connection");
        assert_eq!(group_of(OptTab::Extensions).0, "Extensions & Plugins");
    }

    /// Opening a group lands on its first page, and re-selecting the group you
    /// are already in does not throw away the page you are reading.
    #[test]
    fn selecting_a_group_keeps_the_page_when_it_is_already_open() {
        let target = |group: &str, cur: OptTab| {
            let (_, members) = GROUPS.iter().find(|(n, _)| *n == group).unwrap();
            if members.contains(&cur) {
                cur
            } else {
                members[0]
            }
        };
        assert_eq!(target("Connection", OptTab::General), OptTab::Connection);
        assert_eq!(target("Connection", OptTab::Proxy), OptTab::Proxy);
        assert_eq!(target("Files", OptTab::Proxy), OptTab::FileTypes);
    }

    /// No two pages may share a label: the sub-tab row is the only thing
    /// telling them apart, and the group name is not repeated on it.
    #[test]
    fn every_page_label_is_distinct() {
        for (_, pages) in GROUPS {
            let mut seen: Vec<&str> = pages.iter().map(|p| leaf_label(*p)).collect();
            let before = seen.len();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), before, "duplicate label in {seen:?}");
        }
    }

    /// The group holding the connection pages is named "Connection"; the page
    /// inside it is "Connections". Close, and deliberately so — but the group
    /// name must not be reused verbatim as one of its own sub-tabs, which
    /// would read as a tab containing itself.
    ///
    /// Only groups that actually DRAW a sub-tab row are held to this; a
    /// one-page group draws none, so `Extensions` holding `Extensions` is
    /// never rendered as a tab inside itself.
    #[test]
    fn no_group_name_is_also_one_of_its_page_labels() {
        for (name, pages) in GROUPS.iter().filter(|(_, p)| p.len() > 1) {
            for page in *pages {
                assert_ne!(leaf_label(*page), *name, "{name} contains itself");
            }
        }
    }

    #[test]
    fn the_browser_picker_keeps_the_profile_already_named() {
        assert_eq!(with_browser("", "firefox"), "firefox");
        assert_eq!(with_browser("chrome:Profile 2", "edge"), "edge:Profile 2");
        assert_eq!(with_browser("chrome", "edge"), "edge");
    }

    #[test]
    fn choosing_the_off_entry_clears_the_whole_setting() {
        assert_eq!(with_browser("chrome:Profile 2", &tr(COOKIES_OFF)), "");
        assert_eq!(with_browser("chrome", ""), "");
    }

    #[test]
    fn a_profile_without_a_browser_is_nothing_to_read_from() {
        assert_eq!(with_profile("", "Profile 2"), "");
        assert_eq!(with_profile("chrome", "Profile 2"), "chrome:Profile 2");
        assert_eq!(with_profile("chrome:Profile 2", "  "), "chrome");
        assert_eq!(with_profile("chrome:Profile 2", " Work "), "chrome:Work");
    }

    #[test]
    fn what_is_stored_reads_back_into_the_two_controls() {
        let mut s = crate::model::Settings::default();
        assert_eq!(cookie_browser(&s), tr(COOKIES_OFF));
        assert_eq!(cookie_profile(&s), "");

        s.cookies_from_browser = with_profile(&with_browser("", "chrome"), "Profile 2");
        assert_eq!(s.cookies_from_browser, "chrome:Profile 2");
        assert_eq!(cookie_browser(&s), "chrome");
        assert_eq!(cookie_profile(&s), "Profile 2");
        // And it is a spelling the library parses, which is the point of
        // storing one string rather than two fields.
        assert!(s
            .cookies_from_browser
            .parse::<hya_net::cookies::browser::Source>()
            .is_ok());
    }

    /// The picker offers every browser the library can read, so adding one
    /// there does not silently leave the GUI a browser short.
    #[test]
    fn the_picker_offers_every_browser_the_library_knows() {
        let offered = cookie_browsers();
        assert_eq!(
            offered.len(),
            hya_net::cookies::browser::Browser::ALL.len() + 1
        );
        for b in hya_net::cookies::browser::Browser::ALL {
            assert!(offered.iter().any(|o| o == b.name()), "{b} is missing");
        }
    }

    #[test]
    fn short_paths_are_left_alone() {
        let p = Path::new("/usr/local/bin/ffmpeg");
        assert_eq!(elide_path(p, 58), "/usr/local/bin/ffmpeg");
    }

    #[test]
    fn a_deep_windows_path_keeps_the_drive_and_the_binary() {
        let p = Path::new(
            "C:\\Users\\javad\\AppData\\Local\\Microsoft\\WinGet\\Packages\\Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe\\ffmpeg-7.1-full_build\\bin\\ffmpeg.exe",
        );
        let out = elide_path(p, 58);
        assert!(out.starts_with("C:\\\u{2026}\\"), "lost the drive: {out}");
        assert!(out.ends_with("\\bin\\ffmpeg.exe"), "lost the binary: {out}");
        assert!(out.chars().count() <= 58, "still too long: {out}");
    }

    #[test]
    fn a_deep_unix_path_keeps_its_leading_slash() {
        let p =
            Path::new("/home/javad/.local/share/some/rather/deeply/nested/vendor/tree/bin/ffmpeg");
        let out = elide_path(p, 40);
        assert!(out.starts_with("/\u{2026}/"), "lost the root: {out}");
        assert!(out.ends_with("/bin/ffmpeg"), "lost the binary: {out}");
    }

    #[test]
    fn an_installed_ffmpeg_shows_which_one_and_offers_no_guide() {
        let path = Path::new("/opt/homebrew/bin/ffmpeg");
        let st = FfmpegRow::of(Some(path));
        assert!(st.found);
        assert_eq!(st.badge, tr("Installed"));
        assert_eq!(st.action, tr("Copy path"));
        // Nothing to guide anyone to: it is already here.
        assert_eq!(st.guide, None);
        let (shown, full) = st.path.expect("an installed ffmpeg shows its path");
        assert_eq!(shown, "/opt/homebrew/bin/ffmpeg");
        // The clipboard gets the whole thing, never the elided line.
        assert_eq!(full, "/opt/homebrew/bin/ffmpeg");
    }

    #[test]
    fn a_deep_install_is_elided_for_the_row_but_not_for_the_clipboard() {
        let full = "C:\\Users\\javad\\AppData\\Local\\Microsoft\\WinGet\\Packages\\Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe\\ffmpeg-7.1-full_build\\bin\\ffmpeg.exe";
        let st = FfmpegRow::of(Some(Path::new(full)));
        let (shown, copied) = st.path.expect("an installed ffmpeg shows its path");
        assert!(shown.chars().count() <= 58, "row line too long: {shown}");
        assert!(shown.contains('\u{2026}'), "not elided: {shown}");
        assert_eq!(copied, full);
    }

    #[test]
    fn a_missing_ffmpeg_offers_the_guide_and_shows_no_path() {
        let st = FfmpegRow::of(None);
        assert!(!st.found);
        assert_eq!(st.badge, tr("Not found"));
        assert_eq!(st.guide, Some(FFMPEG_WIKI));
        assert_eq!(st.action, tr("FFmpeg setup guide"));
        assert!(st.path.is_none(), "no path to show, and none shown");
        // The sentence has to say what still works, not just what is absent.
        assert!(st.about.len() > st.badge.len());
    }

    /// Both states build. The row is the one place in this dialog that
    /// changes shape — a third line and a different button — and a widget
    /// tree that does not survive being built takes the whole window down
    /// with it.
    #[test]
    fn both_states_of_the_row_lay_out() {
        let _found: El<'_> = ffmpeg_row(FfmpegRow::of(Some(Path::new("/usr/local/bin/ffmpeg"))));
        let _missing: El<'_> = ffmpeg_row(FfmpegRow::of(None));
    }

    /// Every page builds, on the group it belongs to.
    ///
    /// Splitting one Connection tab into four pages moved four section bodies
    /// between functions, and a `column![]` that lost a bracket in the move
    /// takes the whole window down when the user opens it rather than when the
    /// suite runs. The same reason [`both_states_of_the_row_lay_out`] exists,
    /// applied to the pages the split created.
    #[test]
    fn every_page_lays_out() {
        let app = App::default();
        for (_, pages) in GROUPS {
            for page in *pages {
                let _built: El<'_> = match page {
                    OptTab::General => general(&app),
                    OptTab::FileTypes => file_types(&app),
                    OptTab::SaveTo => save_to(&app),
                    OptTab::Downloads => downloads(&app),
                    OptTab::Connection => conn_limits(&app),
                    OptTab::Cookies => conn_cookies(&app),
                    OptTab::SpeedLimit => conn_speed(&app),
                    OptTab::Quota => conn_quota(&app),
                    OptTab::Proxy => proxy(&app),
                    OptTab::Sites => sites(&app),
                    OptTab::Extensions => extensions(&app),
                    OptTab::MediaTools => media_tools(&app),
                    OptTab::Plugins => crate::plugins::view(&app),
                    OptTab::Sounds => sounds(&app),
                };
            }
        }
    }

    /// The whole window builds on every page, sub-tab row and all — including
    /// the one-page group, where the row is skipped and the two branches of
    /// that decision are what this walks.
    #[test]
    fn the_window_builds_on_every_page() {
        let mut app = App::default();
        for (_, pages) in GROUPS {
            for page in *pages {
                app.options.tab = *page;
                let _window: El<'_> = view(&app);
            }
        }
    }

    /// A page whose list has rows lays out too: the exception and profile
    /// lists are built by a loop that an empty draft never enters.
    #[test]
    fn the_connection_lists_lay_out_with_rows_in_them() {
        let mut app = App::default();
        app.options.draft.conn_exceptions = vec![("mirror.test".into(), 4)];
        app.options.sel_exc = Some(0);
        {
            let _limits: El<'_> = conn_limits(&app);
        }
        app.options.sel_profile = Some(0);
        {
            let _speed: El<'_> = conn_speed(&app);
        }
    }

    #[test]
    fn the_extensions_page_lays_out_its_trusted_origins() {
        let mut app = App::default();
        app.options.draft.allowed_extensions = vec!["moz-extension://a".into()];
        let _page: El<'_> = extensions(&app);
    }

    /// A budget the file name alone cannot meet still shows the file name:
    /// eliding down to "C:\…" would answer nothing.
    #[test]
    fn the_file_name_survives_an_impossible_budget() {
        let p = Path::new("C:\\Program Files\\ffmpeg\\bin\\ffmpeg-with-a-long-name.exe");
        let out = elide_path(p, 10);
        assert_eq!(out, "C:\\\u{2026}\\ffmpeg-with-a-long-name.exe");
    }

    #[test]
    fn the_picker_shows_the_preset_the_box_holds() {
        let chrome = UA_PRESETS.iter().find(|p| p.label == "Chrome").unwrap();
        assert_eq!(ua_preset(chrome.value), Some(*chrome));
        assert_eq!(
            ua_preset(crate::model::DEFAULT_USER_AGENT).map(|p| p.label),
            Some("Default (IE 11)")
        );
    }

    #[test]
    fn an_edited_agent_reads_as_custom() {
        assert_eq!(ua_preset("Mozilla/5.0 (X11)"), None);
        assert_eq!(ua_preset(""), None);
    }

    #[test]
    fn every_preset_is_one_distinct_line_without_hydra() {
        for (i, p) in UA_PRESETS.iter().enumerate() {
            assert!(!p.value.to_ascii_lowercase().contains("hydra"), "{p}");
            assert!(!p.value.contains('\n') && !p.value.contains("  "), "{p}");
            assert!(
                !UA_PRESETS[i + 1..].iter().any(|q| q.value == p.value),
                "{p}"
            );
        }
    }
}
