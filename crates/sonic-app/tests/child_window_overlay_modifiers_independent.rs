//! Regression guard — Haiku review of PR #307 found that the overlay
//! key-intercept branches inside `child_window.rs` (cheat sheet +
//! command palette while the overlay is attached to a child window)
//! were looking up the keymap chord with `self.modifiers`, i.e. the
//! MAIN window's modifier state, instead of the child window's own
//! `child.modifiers`.
//!
//! Concrete symptom: if the user pressed the palette / cheat-sheet
//! toggle chord while a torn-out child window was focused, the chord
//! either silently failed to dispatch (main had no Cmd held) or fired
//! spuriously (main was still holding Cmd from a previous interaction
//! even though the child saw a bare 'p').
//!
//! End-to-end "press a key in a focused child window and observe the
//! palette toggling on/off correctly" requires a live winit event loop
//! plus a real WindowId — same coverage gap documented in
//! `overlay_attaches_to_frontmost.rs`. We pin the fix structurally
//! instead: WindowState owns its own `modifiers` field, the
//! `ModifiersChanged` handler in `child_window.rs` writes to the
//! child's field, and the two overlay intercept branches feed
//! `child_mods` into `key_event_to_string` rather than
//! `self.modifiers`.
//!
//! The structural pin matches the strategy used elsewhere in this
//! crate when a behavior cannot be exercised without a real surface
//! (see e.g. the source-grep guards in `concurrency_invariants.rs`).

use std::fs;
use std::path::PathBuf;

fn child_window_src() -> String {
    let p: PathBuf =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("app").join("child_window.rs");
    fs::read_to_string(&p).expect("child_window.rs readable")
}

fn mod_src() -> String {
    let p: PathBuf =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("app").join("mod.rs");
    fs::read_to_string(&p).expect("app/mod.rs readable")
}

#[test]
fn window_state_owns_its_own_modifier_field() {
    // Per-window modifier state must exist; without this, the only
    // place to read modifiers from in the overlay intercept would be
    // the App-global `self.modifiers`, which is the original bug.
    let s = mod_src();
    assert!(
        s.contains("pub modifiers: ModifiersState"),
        "WindowState must own a `modifiers: ModifiersState` field so child windows track their own modifier state independently of the main window",
    );
}

#[test]
fn modifiers_changed_handler_writes_to_child_not_self() {
    // The ModifiersChanged arm in child_window.rs must write to the
    // child's own field, never to self.modifiers — otherwise a child
    // window's modifier press would update the main window's state
    // and vice versa.
    let s = child_window_src();
    // The handler block (look for the contextual lines):
    let idx = s.find("WindowEvent::ModifiersChanged").expect("handler arm present");
    let window = &s[idx..idx + 200];
    assert!(
        window.contains("child.modifiers = m.state()"),
        "ModifiersChanged in child_window.rs must update `child.modifiers`, found:\n{window}",
    );
    assert!(
        !window.contains("self.modifiers = m.state()"),
        "ModifiersChanged in child_window.rs must NOT write to self.modifiers (would clobber main window's state)",
    );
}

#[test]
fn cheatsheet_overlay_intercept_uses_child_modifiers() {
    let s = child_window_src();
    let idx = s.find("if cheatsheet_here {").expect("cheatsheet_here branch present");
    let window = &s[idx..idx + 1200];
    assert!(
        window.contains("key_event_to_string(&event, child_mods)"),
        "cheatsheet overlay intercept must use child_mods (child's own modifiers) when looking up the chord",
    );
}

#[test]
fn palette_overlay_intercept_uses_child_modifiers() {
    let s = child_window_src();
    // Skip the first occurrence (the read-only borrow at top of the
    // window-event dispatcher) — we want the key-event branch.
    let first = s.find("if palette_here {").expect("palette_here branch present");
    let rest = &s[first + 1..];
    let second = rest.find("if palette_here {").expect("second palette_here branch present");
    let idx = first + 1 + second;
    let window = &s[idx..idx + 1200];
    assert!(
        window.contains("key_event_to_string(&event, child_mods)"),
        "palette overlay intercept must use child_mods (child's own modifiers)",
    );
}

#[test]
fn no_lingering_self_modifiers_in_child_key_paths() {
    // Belt-and-suspenders: the entire keyboard-input dispatch inside
    // child_window.rs must never read `self.modifiers`. Every chord
    // lookup, every encode_key, every overlay branch must use the
    // child's own modifier state. Strip comments before grepping so
    // legitimate prose references to the bug don't trip the check.
    let s = child_window_src();
    let stripped: String = s
        .lines()
        .map(|l| {
            let trimmed = l.trim_start();
            if trimmed.starts_with("//") {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !stripped.contains("self.modifiers"),
        "child_window.rs code must not read `self.modifiers` — key dispatch for a child window MUST source modifiers from `child.modifiers`",
    );
}
