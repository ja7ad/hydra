// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! "Hide from taskbar" on Linux — the X11 half of it.
//!
//! X11 has a window-manager hint for this: `_NET_WM_STATE_SKIP_TASKBAR`
//! (plus `_NET_WM_STATE_SKIP_PAGER`, so the window also leaves the workspace
//! switcher). It is set by sending a `_NET_WM_STATE` client message to the
//! root window, which every EWMH window manager honours — KDE on X11,
//! Cinnamon, Xfce, MATE, i3. winit does not expose the hint, but iced hands
//! us the raw window handle, and x11rb is already in the tree underneath
//! winit, so it costs one extra X connection and nothing at build time.
//!
//! Wayland has no equivalent: a client cannot ask the compositor to keep it
//! out of the dock or the switcher, and no protocol (not even
//! foreign-toplevel) offers one. On a Wayland session the setting therefore
//! only takes effect the way it does on macOS — while Hydra runs in the tray
//! with no window open, nothing shows in the dock or in Alt-Tab.

#![cfg(target_os = "linux")]

use iced::window::raw_window_handle::RawWindowHandle;
use std::sync::OnceLock;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt, EventMask};
use x11rb::rust_connection::RustConnection;
// `change_property32` lives on the wrapper trait, not the protocol one.
use x11rb::wrapper::ConnectionExt as _;

const _NET_WM_STATE_REMOVE: u32 = 0;
const _NET_WM_STATE_ADD: u32 = 1;
/// Source indication: 2 = "a pager or other client", 1 = "normal
/// application". EWMH wants the application value here.
const SOURCE_APPLICATION: u32 = 1;

struct X11 {
    conn: RustConnection,
    root: u32,
    wm_state: u32,
    skip_taskbar: u32,
    skip_pager: u32,
}

fn x11() -> Option<&'static X11> {
    static CONN: OnceLock<Option<X11>> = OnceLock::new();
    CONN.get_or_init(|| match connect() {
        Ok(x) => Some(x),
        Err(e) => {
            crate::log::warn(&format!("hide from taskbar: no X11 connection ({e})"));
            None
        }
    })
    .as_ref()
}

fn connect() -> Result<X11, Box<dyn std::error::Error>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let atom = |name: &str| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
    };
    Ok(X11 {
        wm_state: atom("_NET_WM_STATE")?,
        skip_taskbar: atom("_NET_WM_STATE_SKIP_TASKBAR")?,
        skip_pager: atom("_NET_WM_STATE_SKIP_PAGER")?,
        root,
        conn,
    })
}

/// Add or remove the skip-taskbar hint on one window. Runs inside
/// `iced::window::run`, i.e. on the event-loop thread with the window still
/// alive; a Wayland handle (or any failure) is a silent no-op beyond the log.
pub fn apply(window: &dyn iced::window::Window, hide: bool) {
    let Some(id) = xid(window) else {
        if hide {
            note_wayland();
        }
        return;
    };
    let Some(x) = x11() else { return };
    let action = if hide {
        _NET_WM_STATE_ADD
    } else {
        _NET_WM_STATE_REMOVE
    };
    let event = ClientMessageEvent::new(
        32,
        id,
        x.wm_state,
        [action, x.skip_taskbar, x.skip_pager, SOURCE_APPLICATION, 0],
    );
    // Addressed to the root window: the window manager, not the window
    // itself, owns _NET_WM_STATE.
    let sent = (|| -> Result<(), Box<dyn std::error::Error>> {
        x.conn
            .send_event(
                false,
                x.root,
                EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                event,
            )?
            .check()?;
        x.conn.flush()?;
        Ok(())
    })();
    if let Err(e) = sent {
        crate::log::warn(&format!("hide from taskbar failed: {e}"));
    }
}

/// Mark `child` transient for the window `parent` (an X window id): the
/// window manager keeps it above that window and minimizes it along with
/// it. See `dialog_parent`.
pub fn transient_for(child: &dyn iced::window::Window, parent: u32) {
    let Some(id) = xid(child) else { return };
    let Some(x) = x11() else { return };
    let set = (|| -> Result<(), Box<dyn std::error::Error>> {
        x.conn
            .change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                id,
                x11rb::protocol::xproto::AtomEnum::WM_TRANSIENT_FOR,
                x11rb::protocol::xproto::AtomEnum::WINDOW,
                &[parent],
            )?
            .check()?;
        x.conn.flush()?;
        Ok(())
    })();
    if let Err(e) = set {
        crate::log::warn(&format!("dialog parent: WM_TRANSIENT_FOR failed: {e}"));
    }
}

fn xid(window: &dyn iced::window::Window) -> Option<u32> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window).ok(),
        RawWindowHandle::Xcb(h) => Some(h.window.get()),
        _ => None,
    }
}

fn note_wayland() {
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_ok() {
        crate::log::info(
            "hide from taskbar: Wayland has no skip-taskbar protocol, so open windows stay \
             in the dock; Hydra still leaves it while it runs in the tray with no window open",
        );
    }
}
