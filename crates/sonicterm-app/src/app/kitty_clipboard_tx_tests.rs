use super::*;
use sonicterm_cfg::config::Osc52Policy;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::{CaptureStagingPool, Parser, VtEvent};

/// A parser, one pane's transaction and its private staging: wire bytes go through the real
/// collector, so each test exercises framing, metadata and the transaction together.
struct Rig {
    parser: Parser,
    tx: KittyClipboardTx,
    staging: Arc<ClipboardStaging>,
}

fn rig_with(budget_bytes: usize, limit_bytes: usize, policy: Osc52Policy) -> Rig {
    let staging = ClipboardStaging::with_budget(budget_bytes, limit_bytes);
    staging.set_write_policy(policy);
    let tx = KittyClipboardTx::new(staging.clone(), Arc::default());
    let parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, CaptureStagingPool::new());
    Rig { parser, tx, staging }
}

fn rig() -> Rig {
    rig_with(1 << 20, 1 << 16, Osc52Policy::Allow)
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

impl Rig {
    /// Feed one OSC 5522 body (between `ESC ] 5522;` and ST) and return every step it produced.
    fn send(&mut self, body: &str) -> Vec<KittyStep> {
        self.send_at(body, Instant::now())
    }

    /// [`Self::send`], with the packet arriving at `now`.
    fn send_at(&mut self, body: &str, now: Instant) -> Vec<KittyStep> {
        let events = self.parser.advance(format!("\x1b]5522;{body}\x1b\\").as_bytes());
        let inbox = self.parser.kitty_clipboard_inbox().clone();
        events
            .into_iter()
            .filter_map(|event| match event {
                VtEvent::KittyClipboard { stream, request, bel } => {
                    Some(self.tx.handle(&inbox, stream, request, bel, 0, now))
                }
                _ => None,
            })
            .collect()
    }

    /// The one status reply `body` produced, as text.
    fn reply(&mut self, body: &str) -> String {
        match self.send(body).pop() {
            Some(KittyStep::Reply(bytes)) => String::from_utf8(bytes).expect("ASCII reply"),
            other => panic!("expected a reply to {body:?}, got {other:?}"),
        }
    }

    /// The text of the write `body` committed.
    fn posted(&mut self, body: &str) -> String {
        match self.send(body).pop() {
            Some(KittyStep::Post(job)) => job.text.clone(),
            other => panic!("expected a posted write for {body:?}, got {other:?}"),
        }
    }

    /// Feed a `wdata` chunk of `mime`, expecting no reply.
    fn data(&mut self, mime: &str, payload: &str) {
        let steps = self.send(&format!("type=wdata:mime={};{payload}", b64(mime)));
        assert!(steps.iter().all(|step| matches!(step, KittyStep::Silent)), "{steps:?}");
    }
}

fn status(status: &str, id: &str) -> String {
    let id = if id.is_empty() { String::new() } else { format!(":id={id}") };
    format!("\x1b]5522;type=write:status={status}{id}\x1b\\")
}

/// Contract: every write status echoes the sanitized id of the opening `write`, not of the packet
/// that ended it.
#[test]
fn id_sanitized_and_opening_id_echoed() {
    let mut rig = rig();
    rig.send("type=write:id=a$1");
    assert!(rig.tx.is_open());
    let reply = rig.reply(&format!("type=wdata:id=other:mime={};!!!!", b64("text/plain")));
    assert_eq!(reply, status("EINVAL", "a1"));
    assert!(!rig.tx.is_open());
}

/// Contract: a second `write` silently replaces the open one, releasing what it staged.
#[test]
fn write_replaces_silently() {
    let mut rig = rig();
    rig.send("type=write:id=first");
    rig.data("text/plain", &b64("old"));
    assert!(rig.staging.reserved_bytes() > 0);
    assert!(rig.send("type=write:id=second").iter().all(|step| matches!(step, KittyStep::Silent)));
    assert_eq!(rig.staging.reserved_bytes(), 0);
    assert_eq!(rig.posted("type=wdata"), "");
}

/// Contract: under `osc52 = "deny"` a `write` answers `EPERM` at once and opens nothing, so later
/// `wdata` is ignored.
#[test]
fn deny_at_begin_eperm_then_wdata_ignored() {
    let mut rig = rig_with(1 << 20, 1 << 16, Osc52Policy::Deny);
    assert_eq!(rig.reply("type=write:id=w"), status("EPERM", "w"));
    assert!(!rig.tx.is_open());
    rig.data("text/plain", &b64("x"));
    assert!(rig.send("type=wdata").iter().all(|step| matches!(step, KittyStep::Silent)));
    assert_eq!(rig.staging.reserved_bytes(), 0);
}

/// Contract: one MIME type's payloads form one Base64 stream split anywhere, and a packet ending on
/// padding restarts the stream, so independently padded chunks concatenate.
#[test]
fn stream_split_and_padding_restart() {
    let mut rig = rig();
    rig.send("type=write");
    rig.data("text/plain", "aGVs");
    rig.data("text/plain", "bG8=");
    rig.data("text/plain", &b64(" world"));
    assert_eq!(rig.posted("type=wdata"), "hello world");
}

/// Contract: a stream that ends without its required padding fails the write with `EINVAL` at commit.
#[test]
fn bad_final_padding_einval() {
    let mut rig = rig();
    rig.send("type=write:id=p");
    rig.data("text/plain", "aGk");
    assert_eq!(rig.reply("type=wdata"), status("EINVAL", "p"));
    assert_eq!(rig.staging.reserved_bytes(), 0);
}

/// Contract: switching MIME type finishes the previous stream, which must then be complete; a
/// repeated MIME type restarts its region.
#[test]
fn mime_switch_finishes() {
    let mut rig = rig();
    rig.send("type=write:id=m");
    rig.data("text/plain", "aGk");
    assert_eq!(
        rig.reply(&format!("type=wdata:mime={};aGk=", b64("text/html"))),
        status("EINVAL", "m")
    );
    let mut rig = self::rig();
    rig.send("type=write");
    rig.data("text/plain", &b64("first"));
    rig.data("text/html", &b64("<b>"));
    rig.data("text/plain", &b64("second"));
    assert_eq!(rig.posted("type=wdata"), "second");
}

/// Contract: past 64 MIME entries further types are ignored, and past 64 aliases further aliases
/// are ignored; the transaction itself goes on.
#[test]
fn sixty_fifth_mime_and_alias_ignored() {
    let mut rig = rig();
    rig.send("type=write:id=e");
    for index in 0..64 {
        rig.data(&format!("image/x-{index}"), &b64("i"));
    }
    rig.data("text/plain", &b64("dropped"));
    assert_eq!(rig.reply("type=wdata"), status("ENOSYS", "e"));
    let mut rig = self::rig();
    rig.send("type=write");
    rig.data("image/png", &b64("png"));
    let fillers: Vec<String> = (0..64).map(|index| format!("x/{index}")).collect();
    rig.send(&format!("type=walias:mime={};{}", b64("image/png"), b64(&fillers.join(" "))));
    rig.send(&format!("type=walias:mime={};{}", b64("image/png"), b64("text/plain")));
    assert!(matches!(rig.send("type=wdata").pop(), Some(KittyStep::Reply(_))));
}

/// Contract: aliases resolve over every staged representation before the text is chosen, so an
/// alias naming a text type for a non-text entry still yields text.
#[test]
fn alias_to_non_text_entry_yields_text() {
    let mut rig = rig();
    rig.send("type=write");
    rig.data("application/json", &b64("{\"a\":1}"));
    let steps =
        rig.send(&format!("type=walias:mime={};{}", b64("application/json"), b64("text/plain")));
    assert!(steps.iter().all(|step| matches!(step, KittyStep::Silent)), "{steps:?}");
    assert_eq!(rig.posted("type=wdata"), "{\"a\":1}");
}

/// Contract: aliases apply in order over the evolving map, so a chain resolves through an earlier
/// alias and a later alias overwrites an existing name.
#[test]
fn alias_chain_order() {
    let mut rig = rig();
    rig.send("type=write");
    rig.data("text/html", &b64("<b>x</b>"));
    rig.data("x/source", &b64("source"));
    rig.send(&format!("type=walias:mime={};{}", b64("x/source"), b64("x/middle")));
    rig.send(&format!("type=walias:mime={};{}", b64("x/middle"), b64("text/plain")));
    assert_eq!(rig.posted("type=wdata"), "source");
    let mut rig = self::rig();
    rig.send("type=write:id=a");
    assert_eq!(rig.reply(&format!("type=walias;{}", b64("text/plain"))), status("EINVAL", "a"));
}

/// Contract: a commit with no representation clears the clipboard: it posts an empty write.
#[test]
fn empty_commit_clears_done() {
    let mut rig = rig();
    rig.send("type=write");
    assert_eq!(rig.posted("type=wdata"), "");
}

/// Contract: a commit with representations but none the platform can hold answers `ENOSYS` and
/// releases its staging; `loc=primary` off Linux is unavailable too.
#[test]
fn no_holdable_enosys() {
    let mut rig = rig();
    rig.send("type=write:id=n");
    rig.data("image/png", &b64("png"));
    assert_eq!(rig.reply("type=wdata"), status("ENOSYS", "n"));
    assert_eq!(rig.staging.reserved_bytes(), 0);
    if !cfg!(target_os = "linux") {
        let mut rig = self::rig();
        rig.send("type=write:loc=primary");
        rig.data("text/plain", &b64("p"));
        assert_eq!(rig.reply("type=wdata"), "\x1b]5522;type=write:status=ENOSYS:loc=primary\x1b\\");
    }
}

/// Contract: data past the transaction limit answers `EFBIG` before the buffer grows past it, and
/// the staging is released.
#[test]
fn limit_efbig_before_allocation() {
    let mut rig = rig_with(1 << 20, 8, Osc52Policy::Allow);
    rig.send("type=write:id=f");
    rig.data("text/plain", &b64("12345678"));
    // The lease covers the data buffer, never past the limit, and the MIME name.
    assert!(rig.staging.reserved_bytes() <= 8 + "text/plain".len());
    assert_eq!(
        rig.reply(&format!("type=wdata:mime={};{}", b64("text/plain"), b64("9"))),
        status("EFBIG", "f")
    );
    assert_eq!(rig.staging.reserved_bytes(), 0);
}

/// Contract: data the staging budget cannot cover answers `EBUSY`, reserving nothing.
#[test]
fn budget_ebusy() {
    let mut rig = rig_with(4, 1 << 16, Osc52Policy::Allow);
    rig.send("type=write:id=b");
    assert_eq!(
        rig.reply(&format!("type=wdata:mime={};{}", b64("text/plain"), b64("too long"))),
        status("EBUSY", "b")
    );
    assert_eq!(rig.staging.reserved_bytes(), 0);
}

/// Contract: an overflow or a decoded-field error finishes an open write with `EFBIG` or `EINVAL`
/// only for `wdata`/`walias`; with no write open, or for other operations, nothing answers.
#[test]
fn overflow_and_invalid_value_finish_only_an_open_write() {
    let mut rig = rig();
    let inbox = rig.parser.kitty_clipboard_inbox().clone();
    let now = Instant::now();
    let overflow = KittyClipboardRequest::Overflow(KittyClipboardOperation::Wdata);
    assert!(matches!(rig.tx.handle(&inbox, 1, overflow.clone(), false, 0, now), KittyStep::Silent));
    rig.send("type=write:id=o");
    let invalid = KittyClipboardRequest::InvalidValue(KittyClipboardOperation::Write);
    assert!(matches!(rig.tx.handle(&inbox, 2, invalid, false, 0, now), KittyStep::Silent));
    assert!(rig.tx.is_open());
    match rig.tx.handle(&inbox, 3, overflow, true, 0, now) {
        KittyStep::Reply(bytes) => assert_eq!(bytes, b"\x1b]5522;type=write:status=EFBIG:id=o\x07"),
        other => panic!("expected EFBIG, got {other:?}"),
    }
    assert!(!rig.tx.is_open());
}

/// Contract: with the child silent, an open write expires at its idle deadline (30 s since the
/// last packet) or its absolute deadline (10 min since `write`) with `EBUSY`, releasing it.
#[test]
fn silent_child_idle_and_absolute_deadlines_ebusy() {
    let mut rig = rig();
    rig.send("type=write:id=i");
    rig.data("text/plain", &b64("x"));
    let opened = Instant::now();
    let deadline = rig.tx.deadline().expect("an open write has a deadline");
    assert!(deadline <= opened + KITTY_WRITE_IDLE_TIMEOUT);
    assert!(rig.tx.service(deadline - Duration::from_millis(1)).is_none());
    let reply = rig.tx.service(deadline).expect("expired");
    assert_eq!(reply, status("EBUSY", "i").into_bytes());
    assert_eq!(rig.staging.reserved_bytes(), 0);
    assert!(rig.tx.deadline().is_none());
    // A write kept busy (a packet 5 s before its absolute deadline) still ends at that deadline.
    let start = Instant::now();
    rig.send_at("type=write:id=j", start);
    let late = start + KITTY_WRITE_ABSOLUTE_TIMEOUT - Duration::from_secs(5);
    rig.send_at(&format!("type=wdata:mime={};{}", b64("text/plain"), b64("x")), late);
    let absolute = start + KITTY_WRITE_ABSOLUTE_TIMEOUT;
    assert_eq!(rig.tx.deadline(), Some(absolute));
    assert!(rig.tx.service(absolute - Duration::from_millis(1)).is_none());
    assert_eq!(rig.tx.service(absolute).expect("expired"), status("EBUSY", "j").into_bytes());
}

/// Contract: a policy now `deny` finishes an open write with `EPERM` and releases its staging,
/// even while the child prints nothing; the policy wakeup reaches the worker's receiver.
#[test]
fn policy_revoked_while_child_silent_eperm_releases() {
    let mut rig = rig();
    let wakeups = rig.staging.register_policy_waker();
    rig.send("type=write:id=r");
    rig.data("text/plain", &b64("staged"));
    rig.staging.set_write_policy(Osc52Policy::Deny);
    rig.staging.wake_policy_workers();
    assert!(wakeups.try_recv().is_ok(), "the worker is woken");
    let reply = rig.tx.service(Instant::now()).expect("revoked");
    assert_eq!(reply, status("EPERM", "r").into_bytes());
    assert_eq!(rig.staging.reserved_bytes(), 0);
}

/// Contract: RIS or retire releases an open write silently.
#[test]
fn release_is_silent() {
    let mut rig = rig();
    rig.send("type=write");
    rig.data("text/plain", &b64("x"));
    rig.tx.release();
    assert!(!rig.tx.is_open());
    assert_eq!(rig.staging.reserved_bytes(), 0);
}

/// Contract: a committed write's lease moves into its job and stays reserved until the job is
/// dropped, which the clipboard worker does only after the native call returns.
#[test]
fn lease_through_worker_custody() {
    let mut rig = rig();
    rig.send("type=write");
    rig.data("text/plain", &b64("held"));
    let job = match rig.send("type=wdata").pop() {
        Some(KittyStep::Post(job)) => job,
        other => panic!("expected a posted write, got {other:?}"),
    };
    assert!(rig.staging.reserved_bytes() >= 4);
    assert!(job.kitty.is_some(), "the job carries its reply");
    drop(job);
    assert_eq!(rig.staging.reserved_bytes(), 0);
}

/// Contract: under `ask` a commit takes a consent ticket holding its job; nothing is posted.
#[test]
fn ask_commit_takes_a_ticket() {
    let mut rig = rig_with(1 << 20, 1 << 16, Osc52Policy::Ask);
    rig.send("type=write");
    rig.data("text/plain", &b64("asked"));
    assert!(matches!(rig.send("type=wdata").pop(), Some(KittyStep::Ticket(_))));
}

/// Contract: reads are not served yet: `type=read` answers `EPERM` with its id and terminator.
#[test]
fn read_eperm_before_6b() {
    let mut rig = rig();
    assert_eq!(
        rig.reply("type=read:id=q;dGV4dC9wbGFpbg=="),
        "\x1b]5522;type=read:status=EPERM:id=q\x1b\\"
    );
}

/// Contract: an OSC 5522 write the event loop refuses (`osc52` changed to `deny` after the commit
/// was posted) is answered `EPERM` on its source pane through the sensitive-reply path.
#[test]
fn event_loop_denial_answers_eperm_on_source_pane() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let mut rig = rig();
    rig.send("type=write:id=d");
    rig.data("text/plain", &b64("x"));
    let job = match rig.send("type=wdata").pop() {
        Some(KittyStep::Post(job)) => job,
        other => panic!("expected a posted write, got {other:?}"),
    };
    let mut app = super::super::App::new(Theme::default(), Config::default(), Keymap::default());
    app.config.terminal.osc52 = Osc52Policy::Deny;
    let delivered = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = delivered.clone();
    app.clip_state.reads.port = Some(Box::new(move |pane_id, reply| {
        sink.lock().unwrap().push((pane_id, reply.bytes().to_vec()));
        Ok(())
    }));
    let outcome = app.submit_program_clipboard(7, job, false);
    assert_eq!(outcome, Some(super::super::event_loop::ClipWriteOutcome::Denied));
    assert_eq!(*delivered.lock().unwrap(), vec![(7, status("EPERM", "d").into_bytes())]);
}

/// A native clipboard whose every write panics, as a crashing clipboard worker does.
struct PanickingSink;

impl super::super::clipboard_worker::ClipboardSink for PanickingSink {
    fn write(&mut self, _text: &str, _targets: ClipTargets) -> bool {
        panic!("the clipboard worker dies in its native call");
    }
}

/// Contract: an OSC 5522 write posted to the clipboard worker is answered `EIO` on its source
/// pane when the worker dies before reporting it, never dropped without a status.
#[test]
fn worker_loss_answers_eio_on_source_pane() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let mut rig = rig();
    rig.send("type=write:id=lost");
    rig.data("text/plain", &b64("x"));
    let job = match rig.send("type=wdata").pop() {
        Some(KittyStep::Post(job)) => job,
        other => panic!("expected a posted write, got {other:?}"),
    };
    let mut app = super::super::App::new(Theme::default(), Config::default(), Keymap::default());
    let delivered = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = delivered.clone();
    app.clip_state.reads.port = Some(Box::new(move |pane_id, reply| {
        sink.lock().unwrap().push((pane_id, reply.bytes().to_vec()));
        Ok(())
    }));
    let started =
        app.clip_state.domain.start_worker(Box::new(PanickingSink), None, Box::new(|| {}));
    assert!(started);
    assert_eq!(app.submit_program_clipboard(9, job, false), None, "posted to the worker");
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.clip_state.domain.has_worker() {
        assert!(Instant::now() < deadline, "the worker's loss was never noticed");
        app.drain_clipboard_completions();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(*delivered.lock().unwrap(), vec![(9, status("EIO", "lost").into_bytes())]);
    assert_eq!(rig.staging.reserved_bytes(), 0, "the lost job's lease was released");
}
