//! macOS desktop notifications through `UNUserNotificationCenter`, used only from a bundled app:
//! the center throws in a process with no bundle identifier, such as a `cargo run` binary.

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNUserNotificationCenter,
};

use super::{DeliveryOutcome, DesktopNotice, NotifyBackend};

/// Whether this process may use the notification center: only a bundle has an identifier.
pub(crate) fn has_bundle_identifier() -> bool {
    NSBundle::mainBundle().bundleIdentifier().is_some()
}

/// The user notification center, asked for permission once, on the first request.
pub(crate) struct UserNotificationCenter {
    authorization_requested: bool,
    /// A distinct identifier for each request without one, so none replaces another.
    next_request: u64,
}

impl UserNotificationCenter {
    /// A backend for a bundled app; `None` without a bundle identifier, which leaves
    /// notifications in-app.
    pub(crate) fn new() -> Option<Self> {
        has_bundle_identifier().then_some(Self { authorization_requested: false, next_request: 0 })
    }
}

impl NotifyBackend for UserNotificationCenter {
    fn deliver(&mut self, notice: DesktopNotice, done: Box<dyn FnOnce(DeliveryOutcome) + Send>) {
        let center = UNUserNotificationCenter::currentNotificationCenter();
        // The first request asks permission once; a denial makes later requests fail, which the
        // worker logs, and the in-app notification still shows.
        if !self.authorization_requested {
            self.authorization_requested = true;
            let asked = RcBlock::new(|_granted: Bool, _error: *mut NSError| {});
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &asked,
            );
        }
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&notice.title));
        content.setBody(&NSString::from_str(&notice.body));
        self.next_request += 1;
        let identifier =
            notice.native_id.unwrap_or_else(|| format!("sonicterm-notice-{}", self.next_request));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&identifier),
            &content,
            None,
        );
        // The completion runs once on a queue of the center's choosing; it touches only `done`.
        let done = std::sync::Mutex::new(Some(done));
        let finished = RcBlock::new(move |error: *mut NSError| {
            let outcome = if error.is_null() {
                DeliveryOutcome::Delivered
            } else {
                // When: `error` is set, permission was denied or the request refused.
                DeliveryOutcome::Unavailable("the notification center refused the request")
            };
            if let Some(done) = done.lock().ok().and_then(|mut slot| slot.take()) {
                done(outcome);
            }
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&finished));
    }

    fn quarantines(&self) -> bool {
        // A request handed to the center cannot be withdrawn, so a late one must finish first.
        true
    }
}
