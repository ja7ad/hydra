// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reads the login launch reason before Hydra creates windows or contacts another instance.

use std::cell::RefCell;
use std::sync::Mutex;

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

pub(crate) fn observe() -> oneshot::Receiver<bool> {
    let (sender, receiver) = oneshot::channel();
    let sender = Mutex::new(Some(sender));
    // Iced boots before AppKit processes the launch event. A synchronous
    // notification observer can read it before window creation is scheduled.
    let callback = RcBlock::new(move |_| {
        let event = NSAppleEventManager::sharedAppleEventManager().currentAppleEvent();
        if let Some(sender) = sender.lock().unwrap().take() {
            let _ = sender.send(is_login_launch(event.as_deref()));
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
    fn launch_observer_delivers_once_and_can_be_removed() {
        let mut launched = observe();
        // SAFETY: The name is AppKit's launch notification and the object is absent.
        unsafe {
            NSNotificationCenter::defaultCenter()
                .postNotificationName_object(NSApplicationDidFinishLaunchingNotification, None);
        }
        assert_eq!(launched.try_recv(), Ok(false));
        remove_observer();
        OBSERVER.with(|slot| assert!(slot.borrow().is_none()));
    }
}
