// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Dock-icon visibility on macOS.
//!
//! "Hide Dock icon" switches the NSApplication activation policy between
//! Regular and Accessory. AppKit never gives an Accessory app the system
//! menu bar, and on macOS all of Hydra's menus live there — so going
//! Accessory while a window is open would leave the app unusable (the
//! original hide-Dock bug). The policy is therefore window-aware: Regular
//! whenever any Hydra window is open (Dock tile + menu bar), Accessory only
//! while the app lives in the tray with no windows.

#![cfg(target_os = "macos")]

use std::ffi::CString;

use iced::window::raw_window_handle::RawWindowHandle;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{sel, AnyThread, Encode, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSImage, NSView};
use objc2_foundation::NSData;

use crate::dl_indicator::IconFrame;

extern "C-unwind" fn reopen(
    _delegate: &AnyObject,
    _selector: Sel,
    _application: &AnyObject,
    _has_visible_windows: Bool,
) -> Bool {
    let _ = crate::menubus::sender().send("show_main".into());
    Bool::NO
}

fn install_reopen_handler(class: &AnyClass) {
    let selector = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
    if class.instance_method(selector).is_some() {
        return;
    }
    let encoding = CString::new(format!("{}@:@{}", Bool::ENCODING, Bool::ENCODING))
        .expect("Objective-C encodings contain no null bytes");
    // SAFETY: The callback matches BOOL(id, SEL, id, BOOL); Bool supplies the platform encoding.
    unsafe {
        let implementation: Imp = std::mem::transmute(
            reopen as extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, Bool) -> Bool,
        );
        objc2::ffi::class_addMethod(
            std::ptr::from_ref(class).cast_mut(),
            selector,
            implementation,
            encoding.as_ptr(),
        );
    }
}

pub fn install() {
    let Some(marker) = MainThreadMarker::new() else {
        return;
    };
    let application = NSApplication::sharedApplication(marker);
    let Some(delegate) = application.delegate() else {
        return;
    };
    // SAFETY: The application retains its delegate, whose runtime class is live.
    let class = unsafe { &*objc2::ffi::object_getClass(std::ptr::from_ref(&*delegate).cast()) };
    install_reopen_handler(class);
}

/// Re-assert the policy for the current preference + window state. Call on
/// every window open/close and when the setting changes. Main thread only
/// (the iced update loop qualifies); silently a no-op elsewhere rather than
/// crashing in AppKit.
pub fn sync(hide_dock: bool, windows_open: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let policy = if hide_dock && !windows_open {
        NSApplicationActivationPolicy::Accessory
    } else {
        NSApplicationActivationPolicy::Regular
    };
    if app.activationPolicy() == policy {
        return;
    }
    let _ = app.setActivationPolicy(policy);
    // Accessory -> Regular while a window is up: AppKit only attaches the
    // menu bar on activation, so without this nudge the menus stay missing
    // until the user clicks away and back.
    if policy == NSApplicationActivationPolicy::Regular && windows_open {
        app.activate();
    }
}

/// Draw the download arrow at `frame` on `window`'s minimised Dock tile, or
/// give the tile back to AppKit for [`IconFrame::Idle`]. The app's own Dock
/// icon is left alone: one app has one, and it stays the logo.
pub fn set_miniwindow_icon(window: &dyn iced::window::Window, frame: IconFrame) {
    const SIZE: u32 = 256;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: the pointer is the live NSView of a window iced still owns,
    // read on the main thread inside `window::run`.
    let view: Option<Retained<NSView>> =
        unsafe { Retained::retain(handle.ns_view.as_ptr().cast()) };
    let Some(window) = view.and_then(|view| view.window()) else {
        return;
    };
    let image = (frame != IconFrame::Idle)
        .then(|| crate::icons::download_rgba(frame, None, SIZE, true))
        .and_then(|rgba| crate::icons::png_bytes(&rgba, SIZE, SIZE))
        .and_then(|png| NSImage::initWithData(NSImage::alloc(), &NSData::from_vec(png)));
    window.setMiniwindowImage(image.as_deref());
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::msg_send;
    use objc2::runtime::ClassBuilder;

    #[test]
    fn dock_reopen_events_reach_show_hydra_repeatedly() {
        let superclass = AnyClass::get(c"NSObject").unwrap();
        let class = ClassBuilder::new(c"HydraDockReopenTest", superclass)
            .unwrap()
            .register();
        install_reopen_handler(class);
        install_reopen_handler(class);
        // SAFETY: The test class inherits NSObject's initializer and ownership convention.
        let delegate: Retained<AnyObject> = unsafe { msg_send![class, new] };
        let mut events = crate::menubus::take_events().unwrap();
        for visible in [Bool::NO, Bool::YES, Bool::NO] {
            // SAFETY: The installed method has this signature; the callback does not use the app.
            let default_handling: Bool = unsafe {
                msg_send![&*delegate, applicationShouldHandleReopen: &*delegate,
                    hasVisibleWindows: visible]
            };
            assert!(!default_handling.as_bool());
            assert_eq!(events.try_recv().unwrap(), "show_main");
        }
    }
}
