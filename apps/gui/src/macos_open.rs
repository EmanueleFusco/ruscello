//! Receives Launch Services open requests without replacing the window toolkit's AppKit delegate.

#![allow(unsafe_code)] // The selectors and callbacks below are the native macOS event interface.

use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained, sel,
};
use objc2_app_kit::NSApplicationWillFinishLaunchingNotification;
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSObject,
};
use tokio::sync::mpsc;

const CORE: u32 = u32::from_be_bytes(*b"aevt");
const OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
const GET_URL: u32 = u32::from_be_bytes(*b"GURL");
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const EVENTS: [(u32, u32); 2] = [(CORE, OPEN_DOCUMENTS), (GET_URL, GET_URL)];

define_class!(
    // SAFETY: NSObject supports subclassing, and these selectors have the AppKit/Foundation
    // callback signatures. Launch Services dispatches the events on the application thread.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RuscelloOpenEvents"]
    #[ivars = mpsc::Sender<Vec<String>>]
    struct OpenEventHandler;

    impl OpenEventHandler {
        #[unsafe(method(applicationWillFinishLaunching:))]
        fn application_will_finish_launching(&self, _notification: &NSNotification) {
            // AppKit initializes its own Apple event handlers during launch. Reinstall these two
            // before AppKit dispatches the initial document or URL that launched Ruscello.
            self.register();
        }

        #[unsafe(method(handleOpenEvent:withReplyEvent:))]
        fn handle_open_event(&self, event: &NSAppleEventDescriptor, _reply: Option<&NSAppleEventDescriptor>) {
            // SAFETY: Foundation defines these selectors on an Apple event descriptor, and the
            // direct-object keyword and event ID use the documented four-byte integer ABI.
            let (event_id, payload): (u32, Option<Retained<NSAppleEventDescriptor>>) = unsafe {
                (msg_send![event, eventID], msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT])
            };
            let Some(payload) = payload else {
                return;
            };
            let inputs = if event_id == OPEN_DOCUMENTS {
                (1..=payload.numberOfItems())
                    .filter_map(|index| payload.descriptorAtIndex(index))
                    .filter_map(|item| item.fileURLValue()?.absoluteString())
                    .filter_map(|url| crate::instance::normalize_association(&url.to_string()))
                    .collect()
            } else {
                payload.stringValue()
                    .and_then(|url| crate::instance::normalize_association(&url.to_string()))
                    .into_iter()
                    .collect::<Vec<_>>()
            };
            if !inputs.is_empty() && self.ivars().try_send(inputs).is_err() {
                tracing::debug!("Could not deliver a macOS open request to the desktop queue");
            }
        }
    }
);

impl OpenEventHandler {
    fn register(&self) {
        let manager = NSAppleEventManager::sharedAppleEventManager();
        for (class, id) in EVENTS {
            // SAFETY: The selector is defined above with the two Apple event descriptor arguments
            // required by Foundation. OpenEvents retains this object until it removes the handlers.
            let _: () = unsafe {
                msg_send![&*manager, setEventHandler: self,
                    andSelector: sel!(handleOpenEvent:withReplyEvent:),
                    forEventClass: class, andEventID: id]
            };
        }
    }
}

pub(crate) struct OpenEvents(Retained<OpenEventHandler>);

impl OpenEvents {
    pub(crate) fn install(sender: mpsc::Sender<Vec<String>>) -> anyhow::Result<Self> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("macOS open events require the main thread"))?;
        // SAFETY: Initialize the allocated NSObject subclass once, after setting its Rust ivars.
        let handler: Retained<OpenEventHandler> =
            unsafe { msg_send![super(mtm.alloc().set_ivars(sender)), init] };
        handler.register();
        // SAFETY: The observer is retained by Self until unregistration, the static notification
        // name is initialized by AppKit, and the selector accepts a single NSNotification.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &handler,
                sel!(applicationWillFinishLaunching:),
                Some(NSApplicationWillFinishLaunchingNotification),
                None,
            );
        }
        Ok(Self(handler))
    }
}

impl Drop for OpenEvents {
    fn drop(&mut self) {
        // SAFETY: This is the observer registered by install and it is still retained here.
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(&self.0) };
        let manager = NSAppleEventManager::sharedAppleEventManager();
        for (class, id) in EVENTS {
            // SAFETY: These are the same event class and ID pairs registered by the handler.
            let _: () = unsafe {
                msg_send![&*manager, removeEventHandlerForEventClass: class, andEventID: id]
            };
        }
    }
}
