//! The notice protocol: at most one undelivered event per notice, no lost completion, and
//! installation never clears a live claim.

use super::*;
use std::sync::Mutex as StdMutex;

/// A waker that records every notice id it is called with.
fn recording_waker() -> (FallbackWaker, Arc<StdMutex<Vec<u64>>>) {
    let calls = Arc::new(StdMutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    (Arc::new(move |notice_id| recorded.lock().unwrap().push(notice_id)), calls)
}

#[test]
fn a_completion_before_installation_is_delivered_once_at_installation() {
    // A completion with no waker is owed; installing delivers it once, and a second installation
    // does not deliver it again.
    let notice = FallbackNotice::new();
    notice.complete();
    let (waker, calls) = recording_waker();
    notice.attach_waker(Arc::clone(&waker));
    assert_eq!(*calls.lock().unwrap(), vec![notice.id()]);
    notice.attach_waker(waker);
    assert_eq!(calls.lock().unwrap().len(), 1, "the owed event is delivered once");
}

#[test]
fn a_completion_racing_installation_is_delivered_exactly_once_in_either_order() {
    // The delivery lock serializes them: completion first leaves it owed, installation first lets
    // the completion see the waker. Either way exactly one event is posted.
    for complete_first in [true, false] {
        let notice = FallbackNotice::new();
        let (waker, calls) = recording_waker();
        if complete_first {
            notice.complete();
            notice.attach_waker(waker);
        } else {
            notice.attach_waker(waker);
            notice.complete();
        }
        assert_eq!(calls.lock().unwrap().len(), 1, "complete_first={complete_first}");
        assert!(notice.delivery().posted);
        assert!(!notice.delivery().owed);
    }
}

#[test]
fn completions_around_an_unrelated_frame_leave_at_most_one_undelivered_event() {
    // Several completions before the handler runs post one event; its acknowledgement reads the
    // final generation, and a completion after the acknowledgement posts again.
    let notice = FallbackNotice::new();
    let (waker, calls) = recording_waker();
    notice.attach_waker(waker);
    notice.complete();
    notice.complete();
    notice.complete();
    assert_eq!(calls.lock().unwrap().len(), 1, "one undelivered event");
    assert_eq!(notice.acknowledge(), 3, "the handler sees the final generation");
    notice.complete();
    assert_eq!(calls.lock().unwrap().len(), 2, "after acknowledgement a completion posts again");
}

#[test]
fn installation_while_a_claim_is_posted_leaves_the_claim_alone() {
    // A reinstalled waker does not clear `posted` or post a second event; the queued event keeps
    // the claim until its handler acknowledges it.
    let notice = FallbackNotice::new();
    let (first, first_calls) = recording_waker();
    notice.attach_waker(first);
    notice.complete();
    let (second, second_calls) = recording_waker();
    notice.attach_waker(second);
    assert!(notice.delivery().posted, "the claim stays with its queued event");
    notice.complete();
    assert_eq!((first_calls.lock().unwrap().len(), second_calls.lock().unwrap().len()), (1, 0));
    notice.acknowledge();
    notice.complete();
    assert_eq!(second_calls.lock().unwrap().len(), 1, "the new waker posts after acknowledgement");
}

#[test]
fn each_notice_has_its_own_id_and_events_carry_it() {
    // An old notice and the current one post events naming themselves, so a handler can tell
    // them apart; each keeps its own single undelivered event.
    let old = FallbackNotice::new();
    let current = FallbackNotice::new();
    assert_ne!(old.id(), current.id());
    let (waker, calls) = recording_waker();
    old.attach_waker(Arc::clone(&waker));
    current.attach_waker(waker);
    old.complete();
    current.complete();
    current.complete();
    assert_eq!(*calls.lock().unwrap(), vec![old.id(), current.id()]);
}
