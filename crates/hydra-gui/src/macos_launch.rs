// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reads the login launch reason before Hydra creates windows or contacts another instance.

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::NSApplicationDidFinishLaunchingNotification;
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSNotificationCenter, NSObjectProtocol,
};
use tokio::sync::oneshot;

const OPEN_APPLICATION: u32 = u32::from_be_bytes(*b"oapp");
const LAUNCH_REASON: u32 = u32::from_be_bytes(*b"prop");
const LOGIN_ITEM: u32 = u32::from_be_bytes(*b"lgit");

thread_local! {
    static OBSERVER: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> = const { RefCell::new(None) };
}

fn is_login_launch(event: Option<&NSAppleEventDescriptor>) -> bool {
    event.is_some_and(|event| {
        event.eventID() == OPEN_APPLICATION
            && event
                .paramDescriptorForKeyword(LAUNCH_REASON)
                .is_some_and(|reason| reason.enumCodeValue() == LOGIN_ITEM)
    })
}

/// Longest after the user logs in that loginwindow is still starting login
/// items. It launched Hydra 3.5 s in on the machine this was measured on.
const LOGIN_BURST: Duration = Duration::from_secs(120);

/// The launch belongs to the login that just happened.
///
/// Recent macOS starts an Open at Login item with a plain open-application
/// event, no login reason in it, so the event alone read every login launch
/// as the user opening the app.
fn started_with_login(login: Option<SystemTime>, now: SystemTime) -> bool {
    login.is_some_and(|login| {
        now.duration_since(login)
            .is_ok_and(|since| since <= LOGIN_BURST)
    })
}

/// When the user at the console logged in, from the system's login records.
fn console_login_time() -> Option<SystemTime> {
    let mut login = None;
    // SAFETY: the utmpx database is walked start to end on this one thread,
    // and each record is read before the next call invalidates it.
    unsafe {
        libc::setutxent();
        loop {
            let entry = libc::getutxent();
            if entry.is_null() {
                break;
            }
            let entry = &*entry;
            let line = entry.ut_line.map(|c| c as u8);
            if entry.ut_type == libc::USER_PROCESS && line.starts_with(b"console\0") {
                let secs = u64::try_from(entry.ut_tv.tv_sec).ok();
                login = secs.map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s));
            }
        }
        libc::endutxent();
    }
    login
}

pub(crate) fn observe() -> oneshot::Receiver<bool> {
    let (sender, receiver) = oneshot::channel();
    let sender = Mutex::new(Some(sender));
    // Iced boots before AppKit processes the launch event. A synchronous
    // notification observer can read it before window creation is scheduled.
    let callback = RcBlock::new(move |_| {
        let event = NSAppleEventManager::sharedAppleEventManager().currentAppleEvent();
        if let Some(sender) = sender.lock().unwrap().take() {
            let at_login = is_login_launch(event.as_deref())
                || started_with_login(console_login_time(), SystemTime::now());
            let _ = sender.send(at_login);
        }
    });
    // SAFETY: The notification name is an AppKit constant; no object filter or
    // queue is supplied, and the block captures only a synchronized sender.
    let observer = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(NSApplicationDidFinishLaunchingNotification),
            None,
            None,
            &callback,
        )
    };
    OBSERVER.with(|slot| *slot.borrow_mut() = Some(observer));
    receiver
}

pub(crate) fn remove_observer() {
    OBSERVER.with(|slot| {
        if let Some(observer) = slot.borrow_mut().take() {
            // SAFETY: This is the observer token returned by this notification center.
            unsafe { NSNotificationCenter::defaultCenter().removeObserver((*observer).as_ref()) };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: u32, reason: Option<u32>) -> Retained<NSAppleEventDescriptor> {
        let event = NSAppleEventDescriptor::appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID(
            u32::from_be_bytes(*b"aevt"), id, None, -1, 0,
        );
        if let Some(reason) = reason {
            event.setParamDescriptor_forKeyword(
                &NSAppleEventDescriptor::descriptorWithEnumCode(reason),
                LAUNCH_REASON,
            );
        }
        event
    }

    #[test]
    fn login_launch_requires_an_open_application_event_with_login_reason() {
        assert!(is_login_launch(Some(&event(
            OPEN_APPLICATION,
            Some(LOGIN_ITEM)
        ))));
        assert!(!is_login_launch(Some(&event(OPEN_APPLICATION, None))));
        assert!(!is_login_launch(Some(&event(
            OPEN_APPLICATION,
            Some(u32::from_be_bytes(*b"svit"))
        ))));
        assert!(!is_login_launch(Some(&event(
            u32::from_be_bytes(*b"rapp"),
            Some(LOGIN_ITEM)
        ))));
        assert!(!is_login_launch(None));
    }

    #[test]
    fn a_launch_just_after_the_console_login_is_a_login_launch() {
        let login = SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_462_982);
        let after = |secs| login + Duration::from_secs(secs);

        assert!(started_with_login(Some(login), after(3)));
        assert!(started_with_login(Some(login), after(120)));
        assert!(!started_with_login(Some(login), after(121)));
        assert!(!started_with_login(Some(login), after(8 * 3600)));
        // A clock set back since the login, or no login record at all.
        assert!(!started_with_login(Some(after(60)), login));
        assert!(!started_with_login(None, login));
    }

    #[test]
    fn the_console_login_is_read_from_the_systems_records() {
        // A CI runner has no console session; a desktop's is in the past.
        if let Some(login) = console_login_time() {
            assert!(login <= SystemTime::now());
            assert!(login > SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000));
        }
    }

    #[test]
    fn launch_observer_delivers_once_and_can_be_removed() {
        let mut launched = observe();
        // SAFETY: The name is AppKit's launch notification and the object is absent.
        unsafe {
            NSNotificationCenter::defaultCenter()
                .postNotificationName_object(NSApplicationDidFinishLaunchingNotification, None);
        }
        assert!(launched.try_recv().is_ok());
        assert!(launched.try_recv().is_err());
        remove_observer();
        OBSERVER.with(|slot| assert!(slot.borrow().is_none()));
    }
}
