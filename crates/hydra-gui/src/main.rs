// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! hydra-gui: desktop front end for the hydra download engine.
//!
//! Runs as an iced daemon: the main window plus every dialog (Add URL, File
//! Info, progress, Options, Scheduler, ...) is its own OS window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod autostart;
mod dialog_parent;
mod engine;
mod ext_info;
mod extbus;
mod files;
mod fmt;
mod font;
mod hydata;
mod i18n;
mod icons;
#[cfg(target_os = "linux")]
mod linux_taskbar;
mod log;
#[cfg(target_os = "macos")]
mod macos_dock;
#[cfg(target_os = "macos")]
mod macos_files;
#[cfg(target_os = "macos")]
mod macos_launch;
#[cfg(target_os = "macos")]
mod macos_menu;
#[cfg(target_os = "macos")]
mod macos_surface;
mod menubus;
mod model;
mod nmhost;
mod picker;
mod plugin_link;
mod plugins;
mod proxy;
#[cfg(test)]
mod render_check;
mod scan;
mod sounds;
mod theme;
mod tray;
mod ui;
mod update;
mod windows;

use app::{App, Message, WinKind};
use iced::{window, Subscription, Task, Theme};
use std::ffi::OsString;
use std::path::PathBuf;

/// The directory named by `--config DIR` (or `--config=DIR`), as written on
/// the command line. `Err` carries the line to print before exiting: a
/// misspelt profile path must not fall back to the default one and silently
/// run against the wrong download list.
fn config_dir_arg<I: IntoIterator<Item = OsString>>(args: I) -> Result<Option<PathBuf>, String> {
    // skip(1): argv[0] is the executable, and a portable install may well
    // have put the word "--config" in its path.
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        let value = if arg == *"--config" {
            args.next()
                .ok_or_else(|| "--config needs a directory".to_string())?
        } else if let Some(rest) = arg.to_str().and_then(|a| a.strip_prefix("--config=")) {
            OsString::from(rest)
        } else {
            continue;
        };
        if value.is_empty() {
            return Err("--config needs a directory".into());
        }
        return Ok(Some(PathBuf::from(value)));
    }
    Ok(None)
}

fn plugin_file_arg<I: IntoIterator<Item = OsString>>(args: I) -> Result<Option<PathBuf>, String> {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" || arg == "--debug-plugin-catalog" {
            args.next();
            continue;
        }
        if arg.to_str().is_some_and(|arg| {
            arg.starts_with("--config=")
                || arg.starts_with("--debug-plugin-catalog=")
                || arg.to_ascii_lowercase().starts_with("hydra:")
        }) {
            continue;
        }
        let path = if arg == "--install-plugin" {
            PathBuf::from(
                args.next()
                    .filter(|arg| !arg.is_empty())
                    .ok_or("--install-plugin needs a package file")?,
            )
        } else {
            let path = PathBuf::from(arg);
            if !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("hyaplugin"))
            {
                continue;
            }
            path
        };
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("hyaplugin"))
        {
            return Err("plugin packages must use the .hyaplugin extension".into());
        }
        return std::path::absolute(path)
            .map(Some)
            .map_err(|error| error.to_string());
    }
    Ok(None)
}

fn plugin_link_arg<I: IntoIterator<Item = OsString>>(args: I) -> Result<Option<String>, String> {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" || arg == "--install-plugin" || arg == "--debug-plugin-catalog" {
            args.next();
            continue;
        }
        if let Some(link) = arg
            .to_str()
            .filter(|arg| arg.to_ascii_lowercase().starts_with("hydra:"))
        {
            plugin_link::package_url(link)?;
            return Ok(Some(link.to_string()));
        }
    }
    Ok(None)
}

fn debug_catalog_arg<I: IntoIterator<Item = OsString>>(args: I) -> Result<Option<String>, String> {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" || arg == "--install-plugin" {
            args.next();
            continue;
        }
        let value = if arg == "--debug-plugin-catalog" {
            args.next().and_then(|arg| arg.into_string().ok())
        } else if let Some(value) = arg
            .to_str()
            .and_then(|arg| arg.strip_prefix("--debug-plugin-catalog="))
        {
            Some(value.to_string())
        } else {
            continue;
        };
        if !cfg!(debug_assertions) {
            return Err("--debug-plugin-catalog is only available in debug builds".into());
        }
        return value
            .filter(|value| !value.is_empty())
            .map(Some)
            .ok_or("--debug-plugin-catalog needs an HTTP loopback URL".into());
    }
    Ok(None)
}

/// Make `dir` usable as the application directory: absolute (the login item
/// and the update finisher relaunch this process from an unrelated working
/// directory, so a relative `./profile` has to be pinned down now) and
/// present on disk.
fn prepare_app_dir(dir: PathBuf) -> std::io::Result<PathBuf> {
    let dir = std::path::absolute(dir)?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn main() -> iced::Result {
    // `--config DIR` is resolved before anything else: the single-instance
    // probe below already reads ipc.json out of the application directory.
    match config_dir_arg(std::env::args_os()) {
        Ok(Some(dir)) => match prepare_app_dir(dir) {
            Ok(dir) => model::set_app_dir(dir),
            Err(e) => {
                eprintln!("hydra-gui: --config: {e}");
                std::process::exit(1);
            }
        },
        Ok(None) => {}
        Err(msg) => {
            eprintln!("hydra-gui: {msg}");
            std::process::exit(2);
        }
    }

    match debug_catalog_arg(std::env::args_os()) {
        #[cfg(debug_assertions)]
        Ok(Some(url)) => {
            if let Err(error) = hya_plugin::distribution::configure_debug_catalog(&url) {
                eprintln!("hydra-gui: --debug-plugin-catalog: {error}");
                std::process::exit(2);
            }
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("hydra-gui: {error}");
            std::process::exit(2);
        }
    }

    // From here on a panic lands in this profile's session log.
    log::catch_panics();

    // Single instance: two would fight over state.redb, the tray and
    // ipc.json, so a running one gets the spotlight and this one leaves.
    #[cfg(not(target_os = "macos"))]
    let minimized = std::env::args().any(|a| a == "--minimized");
    let _plugin_file = match plugin_file_arg(std::env::args_os()) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("hydra-gui: {error}");
            std::process::exit(2);
        }
    };
    let _plugin_link = match plugin_link_arg(std::env::args_os()) {
        Ok(link) => link,
        Err(error) => {
            eprintln!("hydra-gui: {error}");
            std::process::exit(2);
        }
    };
    #[cfg(not(target_os = "macos"))]
    if extbus::signal_existing(minimized, _plugin_file.as_deref(), _plugin_link.as_deref()) {
        return Ok(());
    }

    // Software rendering by default: the wgpu/Metal path costs a 100-300 MB
    // baseline where tiny-skia sits in the tens. `renderer = "gpu"` opts in.
    let pre = model::load_config();
    let want_gpu = pre.settings.gpu_render || pre.renderer.as_deref() == Some("gpu");
    if !want_gpu && std::env::var_os("ICED_BACKEND").is_none() {
        std::env::set_var("ICED_BACKEND", "tiny-skia");
    }

    // The platform's own UI face, except Persian and Arabic, which keep the
    // bundled Vazirmatn: per-glyph fallback shaped some runs to nothing.
    iced::daemon(boot, App::update, view)
        .title(title)
        .theme(theme_of)
        .style(style_of)
        // View > Scale is a window scale factor, not a text size: iced grows the
        // whole interface by the ratio, so the rows, buttons and dialog
        // chrome keep the proportions the layout was drawn with.
        .scale_factor(scale_of)
        .subscription(subscription)
        .font(include_bytes!("../assets/fonts/Vazirmatn-Regular.ttf").as_slice())
        .default_font(font::adopt(pre.language.as_deref()))
        .run()
}

fn boot() -> (App, Task<Message>) {
    #[cfg(target_os = "macos")]
    {
        macos_files::install();
        macos_dock::install();
        let launched = macos_launch::observe();
        (
            App {
                cfg: model::load_config(),
                startup_pending: true,
                ..App::default()
            },
            Task::perform(
                async move { launched.await.unwrap_or(false) },
                Message::MacosLaunched,
            ),
        )
    }
    #[cfg(not(target_os = "macos"))]
    boot_with_launch(false)
}

fn starts_in_tray(minimized: bool, login_launch: bool, preference: bool) -> bool {
    minimized || (login_launch && preference)
}

fn boot_with_launch(login_launch: bool) -> (App, Task<Message>) {
    let cfg = model::load_config();
    let minimized = starts_in_tray(
        std::env::args().any(|a| a == "--minimized"),
        login_launch,
        cfg.settings.start_in_tray,
    );
    #[cfg(target_os = "macos")]
    {
        macos_launch::remove_observer();
        let plugin_file = plugin_file_arg(std::env::args_os()).ok().flatten();
        let plugin_link = plugin_link_arg(std::env::args_os()).ok().flatten();
        if extbus::signal_existing(minimized, plugin_file.as_deref(), plugin_link.as_deref()) {
            return (App::default(), iced::exit());
        }
    }
    if let Some(lang) = &cfg.language {
        i18n::set_locale(lang);
    }
    log::init(cfg.log_level.as_deref());
    let state = model::load_state();
    log::banner();

    // The engine thread must exist before the first subscription poll takes
    // its event receiver.
    engine::ensure_started();
    engine::set_power_save(cfg.settings.power_save);
    // The Speed Limiter's cap lives in the engine, not on the specs, so a
    // limiter left switched on has to be put back in force at startup.
    engine::set_global_limit(cfg.settings.global_limit());
    proxy::apply(&cfg.settings);

    // Browser-extension bridge: publish capture settings, then listen for
    // the native-messaging host on a loopback socket (port in ipc.json).
    extbus::publish_config(&cfg);
    extbus::start();
    // Register the native-messaging host with every installed browser, so a
    // fresh install works without anyone running the shell script.
    nmhost::ensure_registered(cfg.settings.portable_capture);

    let quota_saved = (state.dl_quota.used, state.dl_quota.window_start);
    let mut app = App {
        cfg,
        state,
        quota_saved,
        display: app::display_points().unwrap_or(iced::Size::ZERO),
        system_dark: theme::system_is_dark(),
        ..App::default()
    };

    // Queues marked "start on startup" begin running; the first Tick starts
    // their files. Through set_queue_running so paused members are promoted
    // back to Queued — flipping `running` directly left nothing to start.
    let startup_queues: Vec<String> = app
        .cfg
        .queues
        .iter()
        .filter(|q| q.schedule.start_on_startup)
        .map(|q| q.name.clone())
        .collect();
    for name in startup_queues {
        app.set_queue_running(&name, true);
    }

    // Keep the login item in sync with the settings (binary may have moved).
    autostart::apply(
        app.cfg.settings.launch_on_startup,
        app.cfg.settings.start_in_tray,
    );

    // `.old` files a previous update's finisher could not delete (Windows
    // keeps the outgoing exe locked through teardown) get swept now.
    update::sweep_leftovers();

    // Tray availability on Linux is only known after the D-Bus
    // registration below, so this is the intent, not yet the outcome.
    let mut start_hidden = minimized
        && cfg!(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        ));

    if start_hidden {
        // There is no window to install from, so the tray goes up here. On
        // Linux registration is a D-Bus round trip that can also find no
        // watcher at all (a session without status-notifier support, or a
        // login that outran the shell): wait briefly, and open the window
        // after all rather than leave an invisible process behind.
        let queues: Vec<String> = app.cfg.queues.iter().map(|q| q.name.clone()).collect();
        tray::install(&queues, app.cfg.settings.power_save);
        if tray::wait_ready(std::time::Duration::from_secs(5)) {
            log::info("started minimized to tray");
        } else {
            log::warn("no system tray available; opening the main window instead");
            start_hidden = false;
        }
    }
    // Dock visibility before any window opens, so a tray-only launch never
    // flashes a Dock tile. A window about to open forces Regular: Accessory
    // apps get no menu bar, and on macOS every Hydra menu lives there.
    #[cfg(target_os = "macos")]
    macos_dock::sync(app.cfg.settings.hide_from_taskbar, !start_hidden);
    // Startup update check (Options > General), tray launch or not: a
    // machine that boots Hydra into the tray every day is exactly the one
    // that never sees a manual check. A dialog only ever opens on a positive
    // answer; failures are silent — offline is normal.
    let check = if app.cfg.settings.check_updates_on_startup {
        let beta = app.cfg.settings.beta_channel;
        Task::perform(update::check(beta), Message::UpdateChecked)
    } else {
        Task::none()
    };
    let install = plugin_file_arg(std::env::args_os())
        .ok()
        .flatten()
        .map(|path| app.update(Message::InstallPluginFile(path)))
        .unwrap_or_else(Task::none);
    let install_link = plugin_link_arg(std::env::args_os())
        .ok()
        .flatten()
        .and_then(|link| plugin_link::package_url(&link).ok())
        .map(|source| app.update(Message::InstallPluginSource(source)))
        .unwrap_or_else(Task::none);
    let install = Task::batch([install, install_link]);
    if start_hidden {
        (app, Task::batch([check, install, plugins::load()]))
    } else {
        let open_main = app.open_window(WinKind::Main);
        let perm = app.check_folder_access();
        (
            app,
            Task::batch([open_main, perm, check, install, plugins::load()]),
        )
    }
}

fn view(app: &App, id: window::Id) -> app::El<'_> {
    match app.windows.get(&id) {
        Some(WinKind::Main) => windows::main_win::view(app),
        Some(WinKind::AddUrl) => windows::add_url::view(app),
        Some(WinKind::FileInfo(_)) => windows::file_info::view(app),
        Some(WinKind::Progress(dl)) => windows::progress::view(app, *dl),
        Some(WinKind::Complete(dl)) => windows::complete::view(app, *dl),
        Some(WinKind::Options) => windows::options::view(app),
        Some(WinKind::Scheduler) => windows::scheduler::view(app),
        Some(WinKind::Batch) => windows::batch::view(app),
        Some(WinKind::About) => windows::about::view(app),
        Some(WinKind::Shortcuts) => windows::shortcuts::view(app),
        Some(WinKind::Columns) => windows::columns::view(app),
        Some(WinKind::Confirm) => windows::confirm::view(app),
        Some(WinKind::Permissions) => windows::permissions::view(app),
        Some(WinKind::Update) => windows::update::view(app),
        Some(WinKind::Power) => windows::power::view(app),
        Some(WinKind::ZipPreview(_)) => windows::zip_preview::view(app),
        None => iced::widget::container(iced::widget::text(""))
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .style(theme::window)
            .into(),
    }
}

fn title(app: &App, id: window::Id) -> String {
    use crate::i18n::tr;
    match app.windows.get(&id) {
        // Version lives in the About dialog only.
        Some(WinKind::Main) => tr("Hydra Download Manager"),
        Some(WinKind::AddUrl) => tr("Enter new address to download"),
        Some(WinKind::FileInfo(_)) => tr("Download File Info"),
        Some(WinKind::Progress(dl)) => windows::progress::title(app, *dl),
        Some(WinKind::Complete(_)) => tr("Download complete"),
        Some(WinKind::Options) => tr("Hydra Configuration"),
        Some(WinKind::Scheduler) => tr("Scheduler"),
        Some(WinKind::Batch) => tr("Add batch download"),
        Some(WinKind::About) => tr("About Hydra"),
        Some(WinKind::Shortcuts) => tr("Keyboard Shortcuts"),
        Some(WinKind::Columns) => tr("Columns"),
        Some(WinKind::Confirm) => tr("Hydra"),
        Some(WinKind::Permissions) => tr("Permissions"),
        Some(WinKind::Update) => tr("Update Hydra"),
        Some(WinKind::Power) => tr("Hydra"),
        Some(WinKind::ZipPreview(_)) => tr("Zip preview"),
        None => "Hydra".into(),
    }
}

fn theme_of(app: &App, _id: window::Id) -> Theme {
    let dark = match app.cfg.settings.theme() {
        model::ThemeMode::System => app.system_dark,
        model::ThemeMode::Light => false,
        model::ThemeMode::Dark => true,
    };
    if dark {
        Theme::Dark
    } else {
        Theme::Light
    }
}

/// Paint every window surface explicitly: unpainted regions otherwise show
/// through as black bands during resize/tab switches.
/// The View > Scale ratio, applied to every window (see `theme::ui_scale`).
fn scale_of(app: &App, _id: window::Id) -> f32 {
    theme::ui_scale(app.cfg.settings.ui_scale_pct)
}

fn style_of(_app: &App, t: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: theme::window_bg(t),
        text_color: theme::text_color(t),
    }
}

fn subscription(app: &App) -> Subscription<Message> {
    #[cfg(target_os = "macos")]
    if app.startup_pending {
        return Subscription::none();
    }
    let power_save = app.cfg.settings.power_save;
    let mut subs = vec![
        Subscription::run(engine_events).map(Message::Engine),
        Subscription::run(native_menu_events).map(Message::NativeMenu),
        Subscription::run(ext_events).map(Message::Ext),
        Subscription::run(scan_events).map(Message::Scan),
        iced::time::every(std::time::Duration::from_secs(if power_save {
            3
        } else {
            1
        }))
        .map(|_| Message::Tick),
        // OS light/dark flips (View > Theme > System Default); the palette
        // in force at startup is read directly, see `theme::system_is_dark`.
        // Subscribed whatever the setting is, so switching to System Default
        // never has to wait for the next flip to catch up with the desktop.
        iced::system::theme_changes().map(Message::SystemTheme),
        window::close_events().map(Message::WindowClosed),
        window::close_requests().map(Message::WindowCloseRequested),
        // Modifier state for Cmd/Ctrl/Shift multi-selection, and global
        // mouse-up to end a column-resize drag wherever it is released.
        iced::event::listen_with(|event, status, window| match event {
            iced::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(m)) => {
                Some(Message::Mods(m))
            }
            // Shortcuts fire only on events no widget consumed, so typing
            // Cmd+A inside a text field still selects text, not downloads.
            // Every editable combo carries the command modifier; Escape (a
            // dialog's Cancel), Enter (its default button) and Alt+F4 (quit
            // on Windows) are the fixed conventions that do not, and nothing
            // else is passed on — an unconsumed keystroke otherwise costs a
            // full repaint.
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key,
                modified_key,
                physical_key,
                modifiers,
                ..
            }) if status == iced::event::Status::Ignored
                && (modifiers.command()
                    || matches!(
                        key,
                        iced::keyboard::Key::Named(
                            iced::keyboard::key::Named::Escape
                                | iced::keyboard::key::Named::Enter
                                | iced::keyboard::key::Named::F4
                        )
                    )) =>
            {
                let resolved_key =
                    crate::app::resolve_latin_char(&key, &modified_key, physical_key)
                        .map(|c| iced::keyboard::Key::Character(c.to_string().into()))
                        .unwrap_or(key);
                Some(Message::RawKey(resolved_key, modifiers, window))
            }
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                Some(Message::MouseUp)
            }
            iced::Event::Window(iced::window::Event::Moved(p)) => {
                Some(Message::WinMoved(window, p))
            }
            iced::Event::Window(iced::window::Event::Resized(s)) => {
                Some(Message::WinResized(window, s))
            }
            _ => None,
        }),
    ];
    // The power-action countdown shows a number of seconds, so it ticks at
    // one second even in power save — where the main tick is three apart.
    if app.power.is_some() {
        subs.push(iced::time::every(std::time::Duration::from_secs(1)).map(|_| Message::PowerTick));
    }
    // A drag in flight samples the pointer on a fixed cadence instead of on
    // every motion event. Each message repaints the window — on a Retina
    // display macOS colour-converts that whole surface per present — so a
    // 120 Hz pointer meant 120 full repaints a second for a rectangle that
    // reads the same at 60. Power save halves it again.
    if app.list_drag || app.resizing.is_some() || app.header_drag.is_some() {
        subs.push(
            iced::time::every(std::time::Duration::from_millis(if power_save {
                33
            } else {
                16
            }))
            .map(|_| Message::DragTick),
        );
    }
    // The glide tick exists only while something is moving; an idle list
    // costs zero redraws. Power save drops the animation entirely — the bar
    // then steps at the engine's event rate.
    // The marquee of a running virus scan needs the same tick even when no
    // transfer is left moving, so it is part of the condition.
    let progress_open = app
        .windows
        .values()
        .any(|k| matches!(k, WinKind::Progress(_)));
    if !power_save
        && ((progress_open && app.state.downloads.iter().any(|d| d.state.is_active()))
            || app.scanning()
            || app.address_loading())
    {
        subs.push(
            iced::time::every(std::time::Duration::from_millis(
                app.animation_interval_ms(),
            ))
            .map(|_| Message::AnimTick),
        );
    }
    Subscription::batch(subs)
}

/// Engine events as a stream. The receiver can be taken only once; a second
/// subscription instance (which iced never creates for the same id) would
/// simply pend forever.
fn engine_events() -> impl iced::futures::Stream<Item = engine::Event> {
    iced::futures::stream::unfold(engine::take_events(), |rx| async move {
        match rx {
            Some(mut r) => {
                let ev = r.recv().await?;
                Some((ev, Some(r)))
            }
            None => iced::futures::future::pending().await,
        }
    })
}

/// Browser-extension captures arriving over the extbus loopback socket.
fn ext_events() -> impl iced::futures::Stream<Item = extbus::ExtEvent> {
    iced::futures::stream::unfold(extbus::take_events(), |rx| async move {
        match rx {
            Some(mut r) => {
                let ev = r.recv().await?;
                Some((ev, Some(r)))
            }
            None => iced::futures::future::pending().await,
        }
    })
}

/// Console output of the virus scanner running over a finished file.
fn scan_events() -> impl iced::futures::Stream<Item = scan::ScanEvent> {
    iced::futures::stream::unfold(scan::take_events(), |rx| async move {
        match rx {
            Some(mut r) => {
                let ev = r.recv().await?;
                Some((ev, Some(r)))
            }
            None => iced::futures::future::pending().await,
        }
    })
}

/// Menu-bar and tray activations, one stream on every platform (the channel
/// just stays quiet where there are no native menus).
fn native_menu_events() -> impl iced::futures::Stream<Item = String> {
    iced::futures::stream::unfold(menubus::take_events(), |rx| async move {
        match rx {
            Some(mut r) => {
                let id = r.recv().await?;
                Some((id, Some(r)))
            }
            None => iced::futures::future::pending().await,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{config_dir_arg, plugin_file_arg};
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[cfg(target_os = "macos")]
    #[test]
    fn startup_waits_for_appkit_before_subscribing_to_engine_events() {
        let app = super::App {
            startup_pending: true,
            ..super::App::default()
        };
        assert_eq!(super::subscription(&app).units(), 0);
    }

    #[test]
    fn tray_startup_respects_launch_reason_and_preference() {
        for (explicit, login, preference, hidden) in [
            (false, true, true, true),
            (false, true, false, false),
            (false, false, true, false),
            (false, false, false, false),
            (true, false, false, true),
            (true, true, false, true),
            (true, false, true, true),
            (true, true, true, true),
        ] {
            assert_eq!(super::starts_in_tray(explicit, login, preference), hidden);
        }
    }

    fn parse(argv: &[&str]) -> Result<Option<PathBuf>, String> {
        config_dir_arg(argv.iter().map(OsString::from))
    }

    #[test]
    fn plugin_file_arguments_preserve_spaces_and_skip_profile_arguments() {
        let parse = |args: &[&str]| plugin_file_arg(args.iter().map(OsString::from));
        assert!(parse(&["hydra", "--config=profile.hyaplugin"])
            .unwrap()
            .is_none());
        let package = std::path::absolute("some folder/youtube.HYAPLUGIN").unwrap();
        assert_eq!(
            parse(&[
                "hydra-gui",
                "--install-plugin",
                "some folder/youtube.HYAPLUGIN"
            ])
            .unwrap(),
            Some(package.clone())
        );
        assert_eq!(
            parse(&["hydra-gui", "some folder/youtube.HYAPLUGIN"]).unwrap(),
            Some(package)
        );
        assert_eq!(
            parse(&["hydra-gui", "--config", "profile.hyaplugin"]).unwrap(),
            None
        );
        assert_eq!(
            parse(&[
                "hydra-gui",
                "hydra://install-plugin?url=https%3A%2F%2Fexample.com%2Fa.hyaplugin"
            ])
            .unwrap(),
            None
        );
        assert!(parse(&["hydra-gui", "--install-plugin"]).is_err());
        assert!(parse(&["hydra-gui", "--install-plugin", "file.txt"]).is_err());
    }

    #[test]
    #[cfg(debug_assertions)]
    fn debug_catalog_arguments_accept_both_spellings_and_require_a_value() {
        let parse = |args: &[&str]| super::debug_catalog_arg(args.iter().map(OsString::from));
        let url = "http://localhost:8000/plugins.json";
        assert_eq!(
            parse(&["hydra-gui", "--debug-plugin-catalog", url]).unwrap(),
            Some(url.into())
        );
        assert_eq!(
            parse(&[
                "hydra-gui",
                "--debug-plugin-catalog=http://localhost:8000/plugins.json"
            ])
            .unwrap(),
            Some(url.into())
        );
        assert!(parse(&["hydra-gui", "--debug-plugin-catalog"]).is_err());
        assert!(parse(&["hydra-gui", "--debug-plugin-catalog="]).is_err());
        assert_eq!(
            parse(&["hydra-gui", "--config", "--debug-plugin-catalog"]).unwrap(),
            None
        );
        let package_url = "http://localhost:8000/test.hyaplugin";
        assert!(plugin_file_arg(
            ["hydra-gui", "--debug-plugin-catalog", package_url].map(OsString::from)
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn browser_install_arguments_skip_profile_values_and_validate_links() {
        let parse = |args: &[&str]| super::plugin_link_arg(args.iter().map(OsString::from));
        let link = "hydra://install-plugin?url=https%3A%2F%2Fexample.com%2Fa.hyaplugin";
        assert_eq!(parse(&["hydra-gui", link]).unwrap(), Some(link.into()));
        assert_eq!(parse(&["hydra-gui", "--config", link]).unwrap(), None);
        assert_eq!(
            parse(&["hydra-gui", "--config=hydra:profile"]).unwrap(),
            None
        );
        assert_eq!(
            parse(&["hydra-gui", "--install-plugin", "a.hyaplugin"]).unwrap(),
            None
        );
        assert!(parse(&[
            "hydra-gui",
            "hydra://install-plugin?url=http://example.com/a.hyaplugin"
        ])
        .is_err());
    }

    #[test]
    fn no_flag_means_the_platform_directory() {
        assert_eq!(parse(&["hydra-gui", "--minimized"]), Ok(None));
    }

    #[test]
    fn both_spellings_are_accepted() {
        let want = Ok(Some(PathBuf::from("./here")));
        assert_eq!(parse(&["hydra-gui", "--config", "./here"]), want);
        assert_eq!(parse(&["hydra-gui", "--config=./here"]), want);
        assert_eq!(
            parse(&["hydra-gui", "--minimized", "--config", "./here"]),
            want
        );
    }

    #[test]
    fn a_missing_or_empty_directory_is_an_error() {
        assert!(parse(&["hydra-gui", "--config"]).is_err());
        assert!(parse(&["hydra-gui", "--config", ""]).is_err());
        assert!(parse(&["hydra-gui", "--config="]).is_err());
    }

    #[test]
    fn the_executable_path_is_never_read_as_a_flag() {
        assert_eq!(parse(&["/opt/--config=oops/hydra-gui"]), Ok(None));
    }
}
