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
use sonicterm_app::launch_queue::{self, LaunchRequest, OpenScriptRequest};
use sonicterm_types::{CommandLaunch, CwdSource, LaunchOrigin, TerminalLaunch};

use crate::executable_classify::{self, Disposition};

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
            // SAFETY: `self` is stored in `HANDLER` for the life of the process, so the manager's
            // unretained handler reference stays valid; the selector is implemented below.
            unsafe {
                manager.setEventHandler_andSelector_forEventClass_andEventID(
                    self,
                    sel!(handleGetURL:withReplyEvent:),
                    crate::open_urls::INTERNET_EVENT_CLASS,
                    crate::open_urls::GET_URL_EVENT_ID,
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
            let home = sonicterm_app::launch_cli::home_dir();
            let launches: Vec<LaunchRequest> = requests
                .into_iter()
                .flat_map(|request| {
                    launch_requests_for(executable_classify::classify(request), home.clone())
                })
                .collect();
            if !launches.is_empty() {
                let _ = launch_queue::push_requests(launches);
            }
        }

        #[unsafe(method(handleGetURL:withReplyEvent:))]
        fn handle_get_url(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            crate::open_urls::handle_get_url(event);
        }
    }
);

impl OpenDocumentsTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `this` is a freshly allocated `OpenDocumentsTarget` whose ivars
        // are set, and `init` is `NSObject`'s designated initializer, so it runs
        // exactly once on an object that has not been initialised yet.
        unsafe { msg_send![super(this), init] }
    }
}

fn requests_from_event(
    event: &NSAppleEventDescriptor,
    initial_cwd: Option<&std::path::Path>,
) -> Vec<OpenScriptRequest> {
    let Some(list) = event.paramDescriptorForKeyword(keyDirectObject) else {
        // When: the event carries no keyDirectObject parameter, so it names no
        // files at all and there is nothing to open.
        return Vec::new();
    };
    let mut requests = Vec::new();
    for index in 1..=list.numberOfItems() {
        let Some(descriptor) = list.descriptorAtIndex(index) else {
            // When: descriptorAtIndex yields nothing for index, so this slot is
            // skipped instead of abandoning the remaining descriptors.
            continue;
        };
        let Some(url) = descriptor.fileURLValue() else {
            // When: the descriptor is not a file reference, so fileURLValue is
            // absent and there is nothing on disk to resolve.
            continue;
        };
        let Some(original) = url.to_file_path() else {
            // When: to_file_path rejects the url — a non-file scheme or a remote
            // host — so it names no local file this process can open.
            continue;
        };
        let request = if original.is_absolute() {
            OpenScriptRequest::resolve_with_cwd_lookup(original, || None)
        } else {
            // When: original is relative, so it resolves against the working
            // directory captured at launch rather than the current one.
            OpenScriptRequest::resolve_with_cwd_lookup(original, || initial_cwd.map(PathBuf::from))
        };
        match request {
            Ok(request) => requests.push(request),
            Err(error) => tracing::warn!(?error, "macOS open-document path was not resolved"),
        }
    }
    requests
}

/// Turn one classified document open into the requests to queue.
///
/// A script keeps today's draft path. A verified executable runs directly in its
/// canonical parent with `hold` on, so its output stays visible. A refusal opens a
/// plain shell tab beside the file (or in home) and queues a notice; the refusal
/// is logged by reason only, never by path.
pub(crate) fn launch_requests_for(
    disposition: Disposition,
    home: Option<PathBuf>,
) -> Vec<LaunchRequest> {
    match disposition {
        Disposition::Draft(request) => vec![LaunchRequest::Script(request)],
        Disposition::Run { program, cwd, identity } => {
            vec![LaunchRequest::Terminal(TerminalLaunch {
                command: Some(CommandLaunch {
                    program: program.into_os_string(),
                    args: Vec::new(),
                }),
                cwd: Some(cwd),
                cwd_source: Some(CwdSource::Parent),
                title: None,
                hold: true,
                origin: LaunchOrigin::Executable { identity },
            })]
        }
        Disposition::Refuse { refusal, cwd, file_name } => {
            tracing::warn!(
                target: "sonicterm_mac::open_document",
                reason = refusal.as_str(),
                "document open not run"
            );
            let cwd_source = Some(cwd.as_ref().map_or(CwdSource::Home, |_| CwdSource::Parent));
            let shell =
                TerminalLaunch { cwd: cwd.or(home), cwd_source, ..TerminalLaunch::default() };
            vec![LaunchRequest::Terminal(shell), LaunchRequest::Notice(refusal.notice(&file_name))]
        }
    }
}

pub fn install() {
    let mtm = MainThreadMarker::new().expect("open-document observer must install on main thread");
    let _ = INITIAL_CWD.set(std::env::current_dir().ok());
    HANDLER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            // When: slot already holds a target, the observer is installed; a
            // second install must not displace the live handler.
            return;
        }
        let target = OpenDocumentsTarget::new(mtm);
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: `addObserver:selector:name:object:` does not retain the
        // observer, so `target` must outlive the registration; it is stored in
        // `HANDLER` below for the life of the process. The selector is
        // implemented by `OpenDocumentsTarget` and takes one notification.
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
