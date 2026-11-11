// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! macOS + Windows tray backend: `tray-icon` (which re-exports muda as
//! `tray_icon::menu`, so tray and macOS menu bar share one muda instance and
//! therefore one global menu-event channel).

#![cfg(any(target_os = "macos", target_os = "windows"))]

use super::Entry;
use crate::dl_indicator::IconFrame;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

thread_local! {
    static CURRENT: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}
/// Readable from any thread, unlike `CURRENT` — `is_active` answers whether
/// closing the last window may leave the app running.
static INSTALLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The download frame in force and whether it is animated.
    static FRAME: Cell<(IconFrame, bool)> = const { Cell::new((IconFrame::Idle, false)) };
    static PHASE: Cell<u8> = const { Cell::new(0) };
}

/// The shared mono silhouette as a muda icon (see [`crate::icons::logo_mono_rgba`]).
fn mono_icon(white: bool) -> Option<tray_icon::Icon> {
    let (rgba, w, h) = crate::icons::logo_mono_rgba(white)?;
    tray_icon::Icon::from_rgba(rgba, w, h).ok()
}

/// The download arrow for the frame in force, `None` while idle. In its own
/// colours on every platform, so it is never a template or a tinted glyph.
fn download_icon() -> Option<tray_icon::Icon> {
    const SIZE: u32 = 64;
    let (frame, animate) = FRAME.get();
    if frame == IconFrame::Idle {
        return None;
    }
    let sheen = animate.then(|| PHASE.get());
    let rgba = crate::icons::download_rgba(frame, sheen, SIZE, false);
    tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

/// Put the icon for the current frame and phase on the tray, and keep the
/// animation timer running exactly while there is an animated arrow on it.
fn draw() {
    let installed = CURRENT.with(|c| {
        let current = c.borrow();
        let Some(tray) = current.as_ref() else {
            return false;
        };
        #[cfg(target_os = "macos")]
        {
            let (icon, template) = match download_icon() {
                Some(icon) => (Some(icon), false),
                None => (mono_icon(false), true),
            };
            if icon.is_some() {
                let _ = tray.set_icon_with_as_template(icon, template);
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(icon) = download_icon().or_else(|| mono_icon(taskbar_wants_white())) {
            let _ = tray.set_icon(Some(icon));
        }
        true
    });
    let (frame, animate) = FRAME.get();
    ticker::run(installed && animate && frame != IconFrame::Idle);
}

pub fn set_progress(frame: IconFrame, animate: bool) {
    if FRAME.replace((frame, animate)) != (frame, animate) {
        draw();
    }
}

fn tick() {
    PHASE.set((PHASE.get() + 1) % crate::dl_indicator::SHEEN_PHASES);
    draw();
}

/// The animation clock: a timer on the UI thread's own loop, so a step is a
/// tray redraw and nothing else.
#[cfg(target_os = "macos")]
mod ticker {
    use objc2::rc::Retained;
    use objc2_foundation::NSTimer;
    use std::cell::RefCell;
    use std::ptr::NonNull;

    thread_local! {
        static TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
    }

    pub fn run(on: bool) {
        TIMER.with_borrow_mut(|timer| match (on, timer.is_some()) {
            (true, false) => {
                let step = block2::RcBlock::new(|_: NonNull<NSTimer>| super::tick());
                let seconds = f64::from(crate::dl_indicator::SHEEN_INTERVAL_MS) / 1000.0;
                // SAFETY: scheduled on, and only ever fired by, this
                // thread's run loop; the block captures nothing.
                *timer = Some(unsafe {
                    NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, true, &step)
                });
            }
            (false, true) => {
                if let Some(timer) = timer.take() {
                    timer.invalidate();
                }
            }
            _ => {}
        });
    }
}

#[cfg(target_os = "windows")]
mod ticker {
    use std::cell::Cell;
    use std::ffi::c_void;
    use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

    thread_local! {
        static TIMER: Cell<usize> = const { Cell::new(0) };
    }

    unsafe extern "system" fn fire(_: *mut c_void, _: u32, _: usize, _: u32) {
        super::tick();
    }

    pub fn run(on: bool) {
        let running = TIMER.get();
        // SAFETY: a thread timer with no window: its callback runs on this
        // thread's message loop, and the id is the one SetTimer returned.
        unsafe {
            if on && running == 0 {
                TIMER.set(SetTimer(
                    std::ptr::null_mut(),
                    0,
                    crate::dl_indicator::SHEEN_INTERVAL_MS,
                    Some(fire),
                ));
            } else if !on && running != 0 {
                KillTimer(std::ptr::null_mut(), running);
                TIMER.set(0);
            }
        }
    }
}

/// Whether the notification area wants the WHITE glyph.
///
/// `SystemUsesLightTheme`, not `AppsUseLightTheme`. Windows keeps the two
/// apart — Personalization > Colors sets "Windows mode" and "App mode"
/// independently — and the tray icon lives in the TASKBAR, which follows the
/// system one. `dark_light::detect` reads the app value, so a machine with a
/// dark taskbar and light apps got a black glyph painted onto a black
/// taskbar: an icon that is there and cannot be seen.
///
/// A missing value falls back to the app theme rather than to a guess: the
/// key has shipped since Windows 10 1903, and if it is somehow absent the app
/// setting is the best evidence left.
#[cfg(target_os = "windows")]
fn taskbar_wants_white() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(THEME_SUBKEY)
        .and_then(|k| k.get_value::<u32, _>("SystemUsesLightTheme"))
        .map(|light| light == 0)
        .unwrap_or_else(|_| matches!(dark_light::detect(), Ok(dark_light::Mode::Dark)))
}

#[cfg(target_os = "windows")]
const THEME_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// Re-tint the glyph for the taskbar theme in force right now.
///
/// Called on the UI thread — `CURRENT` is thread-local — in answer to
/// [`super::THEME_CHANGED`] from the watcher below.
#[cfg(target_os = "windows")]
pub fn refresh_icon() {
    if FRAME.get().0 != IconFrame::Idle {
        return; // The download arrow carries its own colours.
    }
    let white = taskbar_wants_white();
    CURRENT.with(|c| {
        if let Some(tray) = c.borrow().as_ref() {
            if let Some(icon) = mono_icon(white) {
                crate::log::info(&format!(
                    "tray: glyph now {}",
                    if white { "white" } else { "black" }
                ));
                let _ = tray.set_icon(Some(icon));
            }
        }
    });
}

#[cfg(not(target_os = "windows"))]
pub fn refresh_icon() {
    // macOS renders the glyph as a template image and recolors it itself.
}

/// Watch the theme key and ask the UI thread to re-tint when it changes.
///
/// Its own watch rather than `dark_light::subscribe`, which reports changes
/// to `AppsUseLightTheme` only: switching Windows mode alone leaves that
/// value untouched, so the stream stays silent through exactly the change
/// this icon cares about. `RegNotifyChangeKeyValue` fires on any value in the
/// key; deciding what actually changed is [`taskbar_wants_white`]'s job.
#[cfg(target_os = "windows")]
fn watch_taskbar_theme() {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::Registry::{
        RegNotifyChangeKeyValue, REG_NOTIFY_CHANGE_LAST_SET,
    };
    use winreg::enums::{HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ};

    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    let Ok(key) = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(THEME_SUBKEY, KEY_READ | KEY_NOTIFY)
    else {
        return; // No key to watch: the startup pick stands.
    };
    let tx = crate::menubus::sender();
    let _ = std::thread::Builder::new()
        .name("hydra-tray-theme".into())
        .spawn(move || {
            let mut last = taskbar_wants_white();
            loop {
                // The handle is read from `key` on every pass so the key is
                // provably still alive at the call: closing it is what ends a
                // pending notify, and a watch registered on a closed handle
                // would return at once, forever.
                //
                // SAFETY: an open key this thread owns, a filter constant, no
                // event handle, synchronous — so the call simply blocks until
                // a value under the key is written.
                let status = unsafe {
                    RegNotifyChangeKeyValue(
                        key.raw_handle(),
                        0,
                        REG_NOTIFY_CHANGE_LAST_SET,
                        std::ptr::null_mut(),
                        0,
                    )
                };
                if status != 0 {
                    return;
                }
                let white = taskbar_wants_white();
                if white == last {
                    continue; // Some other personalization value moved.
                }
                last = white;
                if tx.send(super::THEME_CHANGED.to_string()).is_err() {
                    return;
                }
            }
        });
}

/// Render the shared menu model into muda items. Boxed because a submenu's
/// children must outlive the `append_items` call that takes them by
/// reference.
fn render(entries: &[Entry]) -> Vec<Box<dyn IsMenuItem>> {
    entries
        .iter()
        .map(|e| -> Box<dyn IsMenuItem> {
            match e {
                Entry::Separator => Box::new(PredefinedMenuItem::separator()),
                Entry::Item { id, label } => {
                    Box::new(MenuItem::with_id(id.clone(), label, true, None))
                }
                Entry::Check { id, label, checked } => Box::new(CheckMenuItem::with_id(
                    id.clone(),
                    label,
                    true,
                    *checked,
                    None,
                )),
                Entry::Sub { label, items } => {
                    let sub = Submenu::new(label, true);
                    let children = render(items);
                    let refs: Vec<&dyn IsMenuItem> = children.iter().map(|c| c.as_ref()).collect();
                    let _ = sub.append_items(&refs);
                    Box::new(sub)
                }
            }
        })
        .collect()
}

fn build_menu(entries: &[Entry]) -> Menu {
    let menu = Menu::new();
    let children = render(entries);
    let refs: Vec<&dyn IsMenuItem> = children.iter().map(|c| c.as_ref()).collect();
    let _ = menu.append_items(&refs);
    menu
}

pub fn install(entries: Vec<Entry>) {
    let installed = CURRENT.with(|c| c.borrow().is_some());
    if installed {
        return;
    }
    crate::menubus::ensure_menu_handler();
    install_with_menu(build_menu(&entries));
}

pub fn reinstall(entries: Vec<Entry>) {
    CURRENT.with(|c| {
        if let Some(tray) = c.borrow().as_ref() {
            tray.set_menu(Some(Box::new(build_menu(&entries))));
        }
    });
}

pub fn is_active() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

fn install_with_menu(menu: Menu) {
    // Left-click on the icon brings the main window back; the menu is
    // right-click only (see `with_menu_on_left_click` below), matching the
    // Linux backend's `activate`.
    let tx = crate::menubus::sender();
    TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Up,
            ..
        } = ev
        {
            let _ = tx.send(super::ICON_CLICKED.into());
        }
    }));

    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Hydra");
    // tray-icon pops the menu on either button by default, while the Click
    // event fires regardless — so a left-click used to raise the window
    // *behind* an unwanted menu. Left-click activates, right-click opens the
    // menu, as the Windows shell does and as ksni already does on Linux.
    builder = builder.with_menu_on_left_click(false);
    // macOS: a TEMPLATE image — black + alpha that AppKit recolors itself
    // for the light/dark menu bar (and inverts while highlighted). Windows
    // has no template concept, so the colour is picked here and re-picked by
    // `watch_taskbar_theme` whenever the taskbar's own theme changes.
    #[cfg(target_os = "macos")]
    {
        if let Some(icon) = download_icon() {
            builder = builder.with_icon(icon);
        } else if let Some(icon) = mono_icon(false) {
            builder = builder.with_icon(icon).with_icon_as_template(true);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(icon) = download_icon().or_else(|| mono_icon(taskbar_wants_white())) {
            builder = builder.with_icon(icon);
        }
    }
    match builder.build() {
        Ok(tray) => {
            CURRENT.with(|c| *c.borrow_mut() = Some(tray));
            INSTALLED.store(true, Ordering::Relaxed);
            // Downloads that were running before the icon existed.
            draw();
            // Only once there IS an icon to re-tint.
            #[cfg(target_os = "windows")]
            watch_taskbar_theme();
        }
        Err(e) => crate::log::warn(&format!("tray icon unavailable: {e}")),
    }
}
