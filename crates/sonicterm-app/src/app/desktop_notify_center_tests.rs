use std::sync::mpsc;

use super::*;

/// A pending callback the fake center holds until a test answers it.
type Pending<T> = Arc<Mutex<Vec<Box<dyn FnOnce(T) + Send>>>>;

/// A center whose permission prompt and posts wait until the test answers them, recording each
/// post's identifier.
#[derive(Default)]
struct FakeCenter {
    prompts: Pending<bool>,
    posts: Pending<bool>,
    posted_ids: Mutex<Vec<String>>,
}

impl NoticeCenter for FakeCenter {
    fn request_authorization(&self, granted: Box<dyn FnOnce(bool) + Send>) {
        self.prompts.lock().unwrap().push(granted);
    }

    fn add(
        &self,
        _title: &str,
        _body: &str,
        identifier: &str,
        posted: Box<dyn FnOnce(bool) + Send>,
    ) {
        self.posted_ids.lock().unwrap().push(identifier.to_owned());
        self.posts.lock().unwrap().push(posted);
    }
}

/// Answer the oldest waiting callback in `pending` with `value`.
fn answer<T>(pending: &Pending<T>, value: T) {
    let callback = pending.lock().unwrap().remove(0);
    callback(value);
}

/// Deliver `native_id`'s notice through `center` and return the channel its outcome arrives on.
fn deliver(center: &Arc<FakeCenter>, native_id: Option<&str>) -> mpsc::Receiver<DeliveryOutcome> {
    let (sender, outcome) = mpsc::channel();
    let notice =
        DesktopNotice { title: "t".into(), body: "b".into(), native_id: native_id.map(Into::into) };
    deliver_through(center, notice, Box::new(move |result| sender.send(result).unwrap()));
    outcome
}

/// Nothing is posted while the permission prompt is unanswered, and the request stays open, so
/// the worker's bound and quarantine cover it; a later grant posts the same notice.
#[test]
fn a_notice_waits_for_permission_before_it_is_posted() {
    let center = Arc::new(FakeCenter::default());
    let outcome = deliver(&center, Some("job"));
    assert!(center.posted_ids.lock().unwrap().is_empty(), "posted before permission");
    assert!(outcome.try_recv().is_err(), "the request ended before permission was answered");
    answer(&center.prompts, true);
    assert_eq!(*center.posted_ids.lock().unwrap(), ["job"]);
    assert!(outcome.try_recv().is_err(), "the request ended before the post completed");
    answer(&center.posts, true);
    assert_eq!(outcome.try_recv(), Ok(DeliveryOutcome::Delivered));
}

/// A denial posts nothing and ends the request as unavailable; a refused post does too.
#[test]
fn a_denial_or_a_refused_post_is_unavailable() {
    let center = Arc::new(FakeCenter::default());
    let denied = deliver(&center, None);
    answer(&center.prompts, false);
    assert!(center.posted_ids.lock().unwrap().is_empty());
    assert!(matches!(denied.try_recv(), Ok(DeliveryOutcome::Unavailable(_))));
    let refused = deliver(&center, None);
    answer(&center.prompts, true);
    answer(&center.posts, false);
    assert!(matches!(refused.try_recv(), Ok(DeliveryOutcome::Unavailable(_))));
}

/// A notice without an OSC 99 id gets an identifier no other notice shares, so it replaces none;
/// one with an id keeps it, so a later notice with that id replaces it.
#[test]
fn only_a_notice_with_an_id_shares_its_identifier() {
    let center = Arc::new(FakeCenter::default());
    for native_id in [None, None, Some("same"), Some("same")] {
        let _outcome = deliver(&center, native_id);
        answer(&center.prompts, true);
        answer(&center.posts, true);
    }
    let ids = center.posted_ids.lock().unwrap().clone();
    assert_ne!(ids[0], ids[1], "two notices without an id");
    assert!(ids[0].starts_with("sonicterm-") && ids[0].contains("-notice-"));
    assert_eq!(ids[2..], ["same", "same"]);
}
