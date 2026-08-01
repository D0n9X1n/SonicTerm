#![cfg(target_os = "macos")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::NSApplicationWillFinishLaunchingNotification;
use objc2_core_services::{kAEOpenDocuments, kCoreEventClass, keyDirectObject};
use objc2_foundation::{
    MainThreadMarker, NSAppleEventDescriptor, NSAppleEventManager, NSNotification,
    NSNotificationCenter, NSObject, NSObjectProtocol,
};
use sonicterm_app::open_script_bridge::{self, OpenScriptRequest};

thread_local! {
    static HANDLER: RefCell<Option<Retained<OpenDocumentsTarget>>> = const { RefCell::new(None) };
}

static INITIAL_CWD: OnceLock<Option<PathBuf>> = OnceLock::new();

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct OpenDocumentsTarget;

    unsafe impl NSObjectProtocol for OpenDocumentsTarget {}

    impl OpenDocumentsTarget {
        #[unsafe(method(applicationWillFinishLaunching:))]
        fn application_will_finish_launching(&self, _notification: &NSNotification) {
            let manager = NSAppleEventManager::sharedAppleEventManager();
            unsafe {
                manager.setEventHandler_andSelector_forEventClass_andEventID(
                    self,
                    sel!(handleOpenDocuments:withReplyEvent:),
                    kCoreEventClass,
                    kAEOpenDocuments,
                );
            }
        }

        #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
        fn handle_open_documents(
            &self,
            event: &NSAppleEventDescriptor,
            _reply: &NSAppleEventDescriptor,
        ) {
            let requests = requests_from_event(event, INITIAL_CWD.get().and_then(Option::as_deref));
            if !requests.is_empty() {
                let _ = open_script_bridge::push_requests(requests);
            }
        }
    }
);

impl OpenDocumentsTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

fn requests_from_event(
    event: &NSAppleEventDescriptor,
    initial_cwd: Option<&std::path::Path>,
) -> Vec<OpenScriptRequest> {
    let Some(list) = event.paramDescriptorForKeyword(keyDirectObject) else {
        return Vec::new();
    };
    let mut requests = Vec::new();
    for index in 1..=list.numberOfItems() {
        let Some(descriptor) = list.descriptorAtIndex(index) else {
            continue;
        };
        let Some(url) = descriptor.fileURLValue() else {
            continue;
        };
        let Some(original) = url.to_file_path() else {
            continue;
        };
        let request = if original.is_absolute() {
            OpenScriptRequest::resolve_with_cwd_lookup(original, || None)
        } else {
            OpenScriptRequest::resolve_with_cwd_lookup(original, || initial_cwd.map(PathBuf::from))
        };
        match request {
            Ok(request) => requests.push(request),
            Err(error) => tracing::warn!(?error, "macOS open-document path was not resolved"),
        }
    }
    requests
}

pub fn install() {
    let mtm = MainThreadMarker::new().expect("open-document observer must install on main thread");
    let _ = INITIAL_CWD.set(std::env::current_dir().ok());
    HANDLER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return;
        }
        let target = OpenDocumentsTarget::new(mtm);
        let center = NSNotificationCenter::defaultCenter();
        unsafe {
            center.addObserver_selector_name_object(
                &target,
                sel!(applicationWillFinishLaunching:),
                Some(NSApplicationWillFinishLaunchingNotification),
                None,
            );
        }
        *slot = Some(target);
    });
}

#[cfg(test)]
pub(crate) fn requests_from_paths_for_test(paths: &[PathBuf]) -> Vec<OpenScriptRequest> {
    use objc2_core_services::{kAnyTransactionID, kAutoGenerateReturnID};
    use objc2_foundation::NSURL;

    let list = NSAppleEventDescriptor::listDescriptor();
    for (offset, path) in paths.iter().enumerate() {
        let url = NSURL::from_file_path(path).expect("test path must form a file URL");
        let descriptor = NSAppleEventDescriptor::descriptorWithFileURL(&url);
        list.insertDescriptor_atIndex(&descriptor, (offset + 1) as isize);
    }
    let event = NSAppleEventDescriptor::appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID(
        kCoreEventClass,
        kAEOpenDocuments,
        Some(&NSAppleEventDescriptor::currentProcessDescriptor()),
        kAutoGenerateReturnID
            .try_into()
            .expect("auto-generated return ID must fit an i16"),
        kAnyTransactionID,
    );
    event.setParamDescriptor_forKeyword(&list, keyDirectObject);
    requests_from_event(&event, None)
}

#[cfg(test)]
pub(crate) fn resolve_paths(
    paths: impl IntoIterator<Item = PathBuf>,
    initial_cwd: Option<&std::path::Path>,
) -> Vec<OpenScriptRequest> {
    paths
        .into_iter()
        .filter_map(|path| {
            OpenScriptRequest::resolve_with_cwd_lookup(path, || initial_cwd.map(PathBuf::from)).ok()
        })
        .collect()
}

#[cfg(test)]
#[path = "open_documents_tests.rs"]
mod open_documents_tests;
