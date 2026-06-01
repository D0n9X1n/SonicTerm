//! M6c smoke test: confirm the `WindowsShell` boundary that the
//! Windows bin consumes actually wires a caller-built
//! `AppStateMachine` through to state mutation. Routes a synthetic
//! `AppIntent::NewTab` and asserts the reducer bumped `tab_count` +
//! emitted a Render Effect — proves the same machine the shell
//! would hand to the App is reachable + functioning before any
//! winit / Win32 code runs.
//!
//! Pure logic — no Win32 / OLE calls — so this runs on the mac host
//! during local gate as well as on Windows CI.

use sonicterm_app_core::{
    AppEffect, AppIntent, AppState, AppStateMachine, RedrawReason, WindowKey,
};

#[test]
fn state_machine_wired_through_new_tab_intent() {
    // What `crates/sonicterm-windows/src/main.rs` builds before
    // handing to `WindowsShell::new(...)`.
    let mut machine = AppStateMachine::new(AppState::default());
    assert_eq!(machine.state().tab_count, 0, "fresh machine starts with zero tabs");

    let effects = machine.handle(AppIntent::NewTab { window: WindowKey(0), cwd: None });

    assert_eq!(machine.state().tab_count, 1, "NewTab Intent bumps tab_count");
    assert_eq!(machine.state().active_tab_idx, Some(0), "active_tab_idx tracks the new tab");
    assert!(
        effects.iter().any(|e| matches!(
            e,
            AppEffect::Render { window: WindowKey(0), reason: RedrawReason::TabAdded }
        )),
        "NewTab Intent emits a TabAdded Render effect; got {effects:?}"
    );
}

#[test]
fn state_machine_constructible_for_shell_handoff() {
    // The exact constructor `sonicterm-windows::main` calls. If this
    // line stops compiling, the M6c shell handoff is broken at the
    // type level.
    let machine = AppStateMachine::new(AppState::default());
    assert_eq!(machine.state().tab_count, 0);
}
