//! Pins `run_command`'s start on the event loop: a parser another thread holds makes it return
//! `parser_busy` within a bound instead of waiting, with the run reservation released and nothing
//! written, and a write refused after pinning returns within the same bound however busy the
//! parser is.

use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sonicterm_cfg::config::{Config, McpControl};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

use super::*;
use crate::app::mcp::LoopReply;
use crate::app::App;

/// An app with one agent pane at an empty integrated prompt, its PTY a live `cat` (or `cmd`), and
/// every write recorded.
fn agent_pane_at_a_prompt() -> (App, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let agent = app.__test_seed_tab("agent");
    #[cfg(unix)]
    let (program, args) = ("/bin/cat", Vec::<String>::new());
    #[cfg(windows)]
    let (program, args) = ("cmd.exe", vec!["/D".to_owned(), "/Q".to_owned()]);
    let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(program, &args, 80, 24).expect("pty");
    let pane = app.main_mut().unwrap().panes.get_mut(&agent).unwrap();
    pane.pty = Some(pty);
    pane.creator = crate::app::mcp::agents::PaneOwner::agent_if(true);
    pane.parser.lock().advance(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    app.testing.pty_write_log_enabled = true;
    (app, agent)
}

/// Every byte the app wrote to `pane_id`.
fn written(app: &App, pane_id: u64) -> Vec<u8> {
    let writes = app.testing.test_pty_writes.lock();
    writes.iter().filter(|(id, _)| *id == pane_id).flat_map(|(_, bytes)| bytes.clone()).collect()
}

/// Hold `parser` on another thread until the returned sender is used or dropped; returns once the
/// lock is held.
fn hold(parser: &std::sync::Arc<parking_lot::Mutex<sonicterm_vt::vt::Parser>>) -> HeldParser {
    let parser = std::sync::Arc::clone(parser);
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let guard = parser.lock();
        held_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(10)).ok();
        drop(guard);
    });
    held_rx.recv().unwrap();
    HeldParser { release: Some(release_tx), holder: Some(holder) }
}

/// A parser lock held on another thread, released when this is dropped.
struct HeldParser {
    release: Option<mpsc::Sender<()>>,
    holder: Option<std::thread::JoinHandle<()>>,
}

// Lifecycle: HeldParser drop signals its holder thread and joins it, so the lock is free afterwards.
impl Drop for HeldParser {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(holder) = self.holder.take() {
            holder.join().unwrap();
        }
    }
}

/// How long a refusal may take on a loaded CI runner. The code waits `PARSER_WAIT` (5 ms); a
/// macOS runner running the whole suite measured 53 to 70 ms of scheduling delay. A blocking
/// lock would instead wait until the test drops its holder, which it does only after the assert.
const REFUSAL_BOUND: Duration = Duration::from_millis(500);

/// A held parser makes the start return `parser_busy`, distinct from `busy`, after its bounded
/// parser wait instead of blocking until the parser is released, with the run reservation free
/// again and nothing written; once the parser is free the run starts.
#[test]
fn a_busy_parser_refuses_the_start_within_its_bound() {
    let (mut app, agent) = agent_pane_at_a_prompt();
    let parser = std::sync::Arc::clone(&app.main().unwrap().panes[&agent].parser);
    let held = hold(&parser);
    let started = Instant::now();
    let reply = app.mcp_run_start(McpControl::Agent.into(), agent, "ls");
    let elapsed = started.elapsed();
    assert!(matches!(reply, LoopReply::Refused("parser_busy", _)), "{elapsed:?}");
    assert!(elapsed < REFUSAL_BOUND, "the start waited {elapsed:?}");
    let entry = app.mcp_resolve_pane(agent).unwrap();
    assert!(!entry.agent_run.load(Ordering::Acquire), "the reservation was released");
    assert!(written(&app, agent).is_empty(), "nothing was written");
    drop(held);
    let reply = app.mcp_run_start(McpControl::Agent.into(), agent, "ls");
    assert!(matches!(reply, LoopReply::RunStarted(_)), "a free parser starts the run");
    assert_eq!(written(&app, agent), b"ls\r");
}

/// A write refused after the run pinned its prompt, with the parser then held, still returns
/// within the bound with the reservation released and nothing written; the pin it could not
/// release is replaced by the next start, which runs.
#[test]
fn a_refused_write_returns_within_the_bound_and_the_next_start_replaces_the_pin() {
    let (mut app, agent) = agent_pane_at_a_prompt();
    let parser = std::sync::Arc::clone(&app.main().unwrap().panes[&agent].parser);
    // Once the start has pinned, another thread takes the parser and the write is refused, so the
    // cleanup's unpin meets a busy parser.
    let held: std::rc::Rc<std::cell::RefCell<Option<HeldParser>>> = std::rc::Rc::default();
    let held_for_hook = std::rc::Rc::clone(&held);
    let parser_for_hook = std::sync::Arc::clone(&parser);
    AFTER_PIN.set(Some(Box::new(move || {
        *held_for_hook.borrow_mut() = Some(hold(&parser_for_hook));
        true
    })));
    let started = Instant::now();
    let reply = app.mcp_run_start(McpControl::Agent.into(), agent, "ls");
    let elapsed = started.elapsed();
    AFTER_PIN.set(None);
    assert!(matches!(reply, LoopReply::Refused("input_busy", _)), "{elapsed:?}");
    assert!(elapsed < REFUSAL_BOUND, "the cleanup waited {elapsed:?}");
    let entry = app.mcp_resolve_pane(agent).unwrap();
    assert!(!entry.agent_run.load(Ordering::Acquire), "the reservation was released");
    assert!(written(&app, agent).is_empty(), "nothing was admitted");
    drop(held.borrow_mut().take());
    assert!(parser.lock().grid().pinned_run().is_some(), "the busy cleanup kept its pin");
    let reply = app.mcp_run_start(McpControl::Agent.into(), agent, "ls");
    assert!(matches!(reply, LoopReply::RunStarted(_)), "the next start replaces the stale pin");
    assert_eq!(written(&app, agent), b"ls\r");
}
