//! The VT worker's clipboard intake order: each ready chunk first settles the open OSC 52 stream
//! and OSC 5522 write (their deadlines and the policy generation they opened under), then is
//! parsed, so a terminator or commit queued at or past a deadline, or after any policy change,
//! never commits. Every test drives the worker's own ready-output step.

use super::super::spawn_pane_tests::pane_and_worker_handles;
use super::*;
use crate::app::osc52_stream::ClipboardStaging;
use sonicterm_cfg::config::Osc52Policy;

/// An OSC 5522 write opened with one `text/plain` chunk, not yet committed.
const OPEN_WRITE: &[u8] =
    b"\x1b]5522;type=write\x1b\\\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;aGk=\x1b\\";

/// The packet that commits an open OSC 5522 write.
const COMMIT: &[u8] = b"\x1b]5522;type=wdata\x1b\\";

/// A worker whose intake reserves from a private staging under `osc52 = "allow"`.
fn clipboard_worker() -> (PaneState, PaneVtHandles, Arc<ClipboardStaging>) {
    let (pane, handles) = pane_and_worker_handles();
    // A private staging keeps the policy and its generation off the process default.
    let staging = ClipboardStaging::with_budget(1 << 20, 1 << 20);
    staging.set_write_policy(Osc52Policy::Allow);
    handles.clipboard.borrow_mut().replace_staging(staging.clone());
    handles.kitty_clipboard.borrow_mut().replace_staging(staging.clone());
    (pane, handles, staging)
}

/// Take `bytes` as one ready chunk at `now` through the worker's ready-output step; returns
/// whether it posted a clipboard write, and the replies it sent.
fn ready(handles: &PaneVtHandles, now: Instant, bytes: &[u8]) -> (bool, String) {
    let mut written = false;
    let mut replies = Vec::new();
    let mut send_reply = |reply: Vec<u8>| replies.extend(reply);
    handles.take_ready_output(now, &mut send_reply, |send_reply| {
        process_pane_vt_batch_with(
            handles,
            bytes,
            None,
            &mut None,
            &mut SyncLatch::default(),
            |_| None,
            |event| written |= matches!(event, UserEvent::ClipboardWrite { .. }),
            Instant::now,
            send_reply,
        );
    });
    drop(send_reply);
    (written, String::from_utf8_lossy(&replies).into_owned())
}

/// Deny, then allow again, as two quick reloads do: the policy ends where it started, but its
/// generation moved twice.
fn deny_then_allow(staging: &ClipboardStaging) {
    staging.set_write_policy(Osc52Policy::Deny);
    staging.bump_policy_generation();
    staging.set_write_policy(Osc52Policy::Allow);
    staging.bump_policy_generation();
}

/// Contract: an OSC 52 terminator in the first chunk ready after the stream's 60 s expiry never
/// commits: the expiry is applied before that chunk is parsed, so the stream is aborted and the
/// rest of its OSC swallowed.
#[test]
fn osc52_terminator_ready_after_expiry_never_commits() {
    let (_pane, handles, _staging) = clipboard_worker();
    let start = Instant::now();
    assert!(!ready(&handles, start, b"\x1b]52;c;aGk=").0, "the stream is still open");
    let (written, _) = ready(&handles, start + Duration::from_secs(61), b"\x07");
    assert!(!written, "the expired stream never commits");
}

/// Contract: an OSC 5522 commit in the first chunk ready after the write's 30 s idle deadline
/// never commits: the write is finished `EBUSY` before that chunk is parsed.
#[test]
fn kitty_commit_ready_after_idle_deadline_never_commits() {
    let (_pane, handles, _staging) = clipboard_worker();
    let start = Instant::now();
    assert!(!ready(&handles, start, OPEN_WRITE).0, "the write is still open");
    let (written, replies) = ready(&handles, start + Duration::from_secs(31), COMMIT);
    assert!(!written, "the expired write never commits");
    assert!(replies.contains("status=EBUSY"), "{replies:?}");
}

/// Contract: a quick deny then allow while an OSC 52 stream or an OSC 5522 write is open cancels
/// it, though the policy admits writes again: each records the policy generation it opened under,
/// and one whose generation moved ends before the next chunk is parsed (OSC 5522 answers `EPERM`).
#[test]
fn deny_then_allow_cancels_an_open_stream_and_write() {
    let (_pane, handles, staging) = clipboard_worker();
    assert!(!ready(&handles, Instant::now(), b"\x1b]52;c;aGk=").0);
    deny_then_allow(&staging);
    assert!(!ready(&handles, Instant::now(), b"\x07").0, "the OSC 52 stream never commits");

    let (_pane, handles, staging) = clipboard_worker();
    assert!(!ready(&handles, Instant::now(), OPEN_WRITE).0);
    deny_then_allow(&staging);
    let (written, replies) = ready(&handles, Instant::now(), COMMIT);
    assert!(!written, "the OSC 5522 write never commits");
    assert!(replies.contains("status=EPERM"), "{replies:?}");
}

/// The text of every clipboard write `bytes` posts as one ready chunk, in posting order.
fn posted_texts(handles: &PaneVtHandles, bytes: &[u8]) -> Vec<String> {
    let mut texts = Vec::new();
    let mut send_reply = |_: Vec<u8>| {};
    handles.take_ready_output(Instant::now(), &mut send_reply, |send_reply| {
        process_pane_vt_batch_with(
            handles,
            bytes,
            None,
            &mut None,
            &mut SyncLatch::default(),
            |_| None,
            |event| {
                if let UserEvent::ClipboardWrite { job, .. } = event {
                    // Each test job is taken once, here, to read what it would write.
                    let job = job.take().expect("an untaken job");
                    texts.push(job.text.clone());
                }
            },
            Instant::now,
            send_reply,
        );
    });
    texts
}

/// Contract: OSC 52 and OSC 5522 writes in one chunk post in the order they arrived, both ways
/// round, so the last write on the wire is the one left on the clipboard.
#[test]
fn osc52_and_osc5522_writes_post_in_wire_order() {
    // "osc52" is b3NjNTI=; the 5522 write carries "hi".
    let osc52 = b"\x1b]52;c;b3NjNTI=\x07".as_slice();
    let kitty = [OPEN_WRITE, COMMIT].concat();

    let (_pane, handles, _staging) = clipboard_worker();
    let texts = posted_texts(&handles, &[osc52, kitty.as_slice()].concat());
    assert_eq!(texts, ["osc52", "hi"], "OSC 52 first on the wire posts first");

    let (_pane, handles, _staging) = clipboard_worker();
    let texts = posted_texts(&handles, &[kitty.as_slice(), osc52].concat());
    assert_eq!(texts, ["hi", "osc52"], "OSC 5522 first on the wire posts first");
}
