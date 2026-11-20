//! The order a permission-gated notification center needs: ask for permission, and post only
//! once it is granted, within the same bounded request. Kept apart from the macOS calls so the
//! order is tested on every host with a fake center.

use std::sync::{Arc, Mutex};

use super::{notice_identifier, DeliveryOutcome, DesktopNotice};

/// One request's completion, taken by whichever callback ends the request first.
type Completion = Arc<Mutex<Option<Box<dyn FnOnce(DeliveryOutcome) + Send>>>>;

/// A notification center that grants permission before it posts. Each callback runs once, on a
/// thread of the center's choosing.
pub(crate) trait NoticeCenter: Send + Sync + 'static {
    /// Ask for permission; `granted` receives the answer, at once when the user already decided.
    fn request_authorization(&self, granted: Box<dyn FnOnce(bool) + Send>);

    /// Post `title` and `body` under `identifier`; `posted` receives whether the center took it.
    fn add(&self, title: &str, body: &str, identifier: &str, posted: Box<dyn FnOnce(bool) + Send>);
}

/// Deliver `notice` through `center`: permission first, then the post, and `done` once with the
/// outcome. An unanswered prompt leaves `done` waiting, so the worker's bound covers it.
pub(crate) fn deliver_through(
    center: &Arc<impl NoticeCenter>,
    notice: DesktopNotice,
    done: Box<dyn FnOnce(DeliveryOutcome) + Send>,
) {
    let identifier = notice_identifier(&notice);
    let completion: Completion = Arc::new(Mutex::new(Some(done)));
    let poster = Arc::clone(center);
    center.request_authorization(Box::new(move |granted| {
        if !granted {
            // When: permission is not `granted` (denied, or asked and refused), nothing is posted.
            finish(&completion, DeliveryOutcome::Unavailable("notifications are not permitted"));
            return;
        }
        let posted_completion = Arc::clone(&completion);
        poster.add(
            &notice.title,
            &notice.body,
            &identifier,
            Box::new(move |posted| {
                let outcome = if posted {
                    DeliveryOutcome::Delivered
                } else {
                    // When: `posted` is false, the center refused it; the in-app notification stands alone.
                    DeliveryOutcome::Unavailable("the notification center refused the request")
                };
                finish(&posted_completion, outcome);
            }),
        );
    }));
}

/// End the request with `outcome`, unless an earlier callback already ended it.
fn finish(completion: &Completion, outcome: DeliveryOutcome) {
    if let Some(done) = completion.lock().ok().and_then(|mut slot| slot.take()) {
        done(outcome);
    }
}

#[cfg(test)]
#[path = "desktop_notify_center_tests.rs"]
mod desktop_notify_center_tests;
