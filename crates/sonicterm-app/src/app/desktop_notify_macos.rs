//! macOS desktop notifications through `UNUserNotificationCenter`, used only from a bundled app:
//! the center throws in a process with no bundle identifier, such as a `cargo run` binary.

use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNUserNotificationCenter,
};

use super::center::{deliver_through, NoticeCenter};
use super::{DeliveryOutcome, DesktopNotice, NotifyBackend};

/// Whether this process may use the notification center: only a bundle has an identifier.
pub(crate) fn has_bundle_identifier() -> bool {
    NSBundle::mainBundle().bundleIdentifier().is_some()
}

/// The process's `UNUserNotificationCenter`, fetched per call because its object is not `Send`.
pub(crate) struct UserNotifications;

/// Wrap a once-only callback for a block the center may call from any queue.
fn once<T: 'static>(callback: Box<dyn FnOnce(T) + Send>) -> impl Fn(T) + 'static {
    let slot = Mutex::new(Some(callback));
    move |value| {
        if let Some(callback) = slot.lock().ok().and_then(|mut slot| slot.take()) {
            callback(value);
        }
    }
}

impl NoticeCenter for UserNotifications {
    fn request_authorization(&self, granted: Box<dyn FnOnce(bool) + Send>) {
        // The center prompts only while the user has not decided, and otherwise answers at once
        // with the stored decision, so asking before every post never prompts twice.
        let granted = once(granted);
        let answered =
            RcBlock::new(move |allowed: Bool, _error: *mut NSError| granted(allowed.as_bool()));
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &answered,
            );
    }

    fn add(&self, title: &str, body: &str, identifier: &str, posted: Box<dyn FnOnce(bool) + Send>) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(identifier),
            &content,
            None,
        );
        let posted = once(posted);
        let finished = RcBlock::new(move |error: *mut NSError| posted(error.is_null()));
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&finished));
    }
}

/// The user notification center behind the worker; permission is asked inside each request.
pub(crate) struct UserNotificationCenter {
    center: Arc<UserNotifications>,
}

impl UserNotificationCenter {
    /// A backend for a bundled app; `None` without a bundle identifier, which leaves
    /// notifications in-app.
    pub(crate) fn new() -> Option<Self> {
        has_bundle_identifier().then(|| Self { center: Arc::new(UserNotifications) })
    }
}

impl NotifyBackend for UserNotificationCenter {
    fn deliver(&mut self, notice: DesktopNotice, done: Box<dyn FnOnce(DeliveryOutcome) + Send>) {
        deliver_through(&self.center, notice, done);
    }

    fn quarantines(&self) -> bool {
        // A permission prompt or a post handed to the center cannot be withdrawn, so a late one
        // must finish before another is sent.
        true
    }
}
