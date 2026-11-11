// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Download progress on the main window's taskbar button
//! (`ITaskbarList3::SetProgressState` / `SetProgressValue`).
//!
//! windows-sys carries no COM interfaces, so the vtable is spelled out here
//! as far as the two methods used.

#![cfg(target_os = "windows")]

use crate::dl_indicator::Indicator;
use iced::window::raw_window_handle::RawWindowHandle;
use std::cell::Cell;
use std::ffi::c_void;
use windows_sys::core::{GUID, HRESULT};
use windows_sys::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows_sys::Win32::UI::Shell::{
    TaskbarList, TBPFLAG, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL,
};

const IID_ITASKBARLIST3: GUID = GUID::from_u128(0xea1afb91_9e28_4b86_90e9_9e9f8a5eefaf);

/// `ITaskbarList3` in vtable order: `IUnknown`, `ITaskbarList`,
/// `ITaskbarList2`, then the first two methods of `ITaskbarList3`.
#[repr(C)]
struct Vtbl {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    hr_init: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    add_tab: usize,
    delete_tab: usize,
    activate_tab: usize,
    set_active_alt: usize,
    mark_fullscreen_window: usize,
    set_progress_value: unsafe extern "system" fn(*mut c_void, *mut c_void, u64, u64) -> HRESULT,
    set_progress_state: unsafe extern "system" fn(*mut c_void, *mut c_void, TBPFLAG) -> HRESULT,
}

thread_local! {
    /// The taskbar object, created on first use and kept for the life of the
    /// UI thread. Null while unavailable (no shell, as under some remote
    /// sessions); creation is retried on the next change.
    static TASKBAR: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
}

fn taskbar() -> Option<*mut c_void> {
    let known = TASKBAR.get();
    if !known.is_null() {
        return Some(known);
    }
    let mut object: *mut c_void = std::ptr::null_mut();
    // SAFETY: winit initialises COM on the UI thread this runs on; the out
    // pointer is a live local and is only trusted on S_OK.
    let created = unsafe {
        CoCreateInstance(
            &TaskbarList,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ITASKBARLIST3,
            &mut object,
        )
    };
    if created < 0 || object.is_null() {
        return None;
    }
    // SAFETY: a COM object's first word is its vtable pointer, and the
    // interface asked for is the one `Vtbl` lays out.
    if unsafe { ((**object.cast::<*const Vtbl>()).hr_init)(object) } < 0 {
        return None;
    }
    TASKBAR.set(object);
    Some(object)
}

/// The taskbar flag and the completed part out of 100 for an indicator.
fn state(shown: Indicator) -> (TBPFLAG, u64) {
    match shown {
        Indicator::Idle => (TBPF_NOPROGRESS, 0),
        Indicator::Indeterminate => (TBPF_INDETERMINATE, 0),
        Indicator::Percent(p) => (TBPF_NORMAL, u64::from(p.min(100))),
    }
}

/// Show `shown` on `window`'s taskbar button. Runs inside
/// `iced::window::run`, on the UI thread with the window alive.
pub fn apply(window: &dyn iced::window::Window, shown: Indicator) {
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let Some(object) = taskbar() else {
        return;
    };
    let hwnd = handle.hwnd.get() as *mut c_void;
    let (flag, done) = state(shown);
    // SAFETY: `object` is the live ITaskbarList3 from `taskbar`, called on
    // the thread that created it; a stale HWND is rejected by the shell.
    unsafe {
        let vtbl = &**object.cast::<*const Vtbl>();
        (vtbl.set_progress_state)(object, hwnd, flag);
        if flag == TBPF_NORMAL {
            (vtbl.set_progress_value)(object, hwnd, done, 100);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indicators_map_to_taskbar_states() {
        assert_eq!(state(Indicator::Idle), (TBPF_NOPROGRESS, 0));
        assert_eq!(state(Indicator::Indeterminate), (TBPF_INDETERMINATE, 0));
        assert_eq!(state(Indicator::Percent(42)), (TBPF_NORMAL, 42));
        assert_eq!(state(Indicator::Percent(130)), (TBPF_NORMAL, 100));
    }

    #[test]
    fn the_vtable_puts_the_progress_calls_in_slots_nine_and_ten() {
        let word = std::mem::size_of::<usize>();
        assert_eq!(std::mem::offset_of!(Vtbl, set_progress_value), 9 * word);
        assert_eq!(std::mem::offset_of!(Vtbl, set_progress_state), 10 * word);
    }
}
