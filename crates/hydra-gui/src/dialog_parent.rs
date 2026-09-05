// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Dialogs stay above the main window.
//!
//! iced opens every window as an independent top-level one, so a click on
//! the main window sent the Add URL or Options dialog behind it, where it
//! was easy to lose and easy to open twice. Each desktop has an "owned
//! window" notion that keeps a window in front of the one it belongs to,
//! and moves and minimizes it with that one; winit does not expose it, but
//! iced hands out the raw window handles, so it is set here after the
//! dialog opens:
//!
//! * macOS: the dialog becomes a child `NSWindow` of the main one.
//! * Windows: the main window becomes the dialog's owner (`GWLP_HWNDPARENT`).
//! * Linux/X11: `WM_TRANSIENT_FOR`, which every window manager honours.
//!   Wayland has no way for a client to name a parent for an existing
//!   toplevel, so there the dialog stays an ordinary window.
//!
//! Two steps because `iced::window::run` reaches one window at a time: a
//! [`token`] is read off the main window, then handed to [`attach`] on the
//! dialog. Both run on the event-loop thread; the token is a plain handle
//! value (an `NSWindow` pointer, an `HWND`, an X window id) and is only ever
//! used on that same thread.

use iced::window::raw_window_handle::RawWindowHandle;

/// The main window, as [`attach`] wants it. `None` for a handle this
/// platform cannot parent under (a Wayland surface).
pub fn token(window: &dyn iced::window::Window) -> Option<usize> {
    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(h) => {
            let view = unsafe { ns_view(h.ns_view.as_ptr()) }?;
            let window = view.window()?;
            Some(objc2::rc::Retained::as_ptr(&window) as usize)
        }
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as usize),
        #[cfg(target_os = "linux")]
        RawWindowHandle::Xlib(h) => usize::try_from(h.window).ok(),
        #[cfg(target_os = "linux")]
        RawWindowHandle::Xcb(h) => Some(h.window.get() as usize),
        _ => None,
    }
}

/// Put `child` under the window [`token`] named. A parent that has gone
/// away in the meantime, or a handle of another kind, leaves the dialog an
/// ordinary window — never a crash.
pub fn attach(child: &dyn iced::window::Window, parent: usize) {
    let Ok(handle) = child.window_handle() else {
        return;
    };
    match handle.as_raw() {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(h) => {
            use objc2_app_kit::{NSApplication, NSWindowOrderingMode};
            let Some(mtm) = objc2::MainThreadMarker::new() else {
                return;
            };
            // The pointer is only trusted if AppKit still lists that
            // window: the main window can close to the tray between the
            // two steps.
            let parent = NSApplication::sharedApplication(mtm)
                .windows()
                .iter()
                .find(|w| objc2::rc::Retained::as_ptr(w) as usize == parent);
            let Some(parent) = parent else {
                return;
            };
            let Some(view) = (unsafe { ns_view(h.ns_view.as_ptr()) }) else {
                return;
            };
            let Some(child) = view.window() else {
                return;
            };
            if objc2::rc::Retained::as_ptr(&child) == objc2::rc::Retained::as_ptr(&parent) {
                return;
            }
            // SAFETY: both are live NSWindows on the main thread, and the
            // child is not the parent. AppKit detaches the child again when
            // it is ordered out (closed), so no bookkeeping is owed here.
            unsafe {
                parent.addChildWindow_ordered(&child, NSWindowOrderingMode::Above);
            }
            crate::log::debug("dialog kept above the main window");
        }
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(h) => {
            use windows_sys::Win32::UI::WindowsAndMessaging::{SetWindowLongPtrW, GWLP_HWNDPARENT};
            let child = h.hwnd.get();
            if child as usize == parent {
                return;
            }
            // SAFETY: plain Win32 call on a window this process owns; an
            // owner that no longer exists is rejected by the system, not
            // dereferenced.
            unsafe {
                SetWindowLongPtrW(child, GWLP_HWNDPARENT, parent as isize);
            }
            crate::log::debug("dialog kept above the main window");
        }
        #[cfg(target_os = "linux")]
        RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => {
            if let Ok(parent) = u32::try_from(parent) {
                crate::linux_taskbar::transient_for(child, parent);
            }
        }
        _ => {}
    }
}

/// The `NSView` behind an AppKit handle.
///
/// SAFETY (caller): `ptr` comes from a raw window handle iced hands out for
/// a live window, on the main thread.
#[cfg(target_os = "macos")]
unsafe fn ns_view(
    ptr: *mut std::ffi::c_void,
) -> Option<objc2::rc::Retained<objc2_app_kit::NSView>> {
    unsafe { objc2::rc::Retained::retain(ptr.cast()) }
}
