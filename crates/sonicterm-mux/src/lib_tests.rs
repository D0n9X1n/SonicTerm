//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::{ClientMsg, ServerMsg};

#[test]
fn exports_protocol_messages() {
    let spawn = ClientMsg::Spawn { cmd: "sh".to_string(), cols: 80, rows: 24 };
    assert!(matches!(spawn, ClientMsg::Spawn { cols: 80, rows: 24, .. }));

    let sessions = ServerMsg::Sessions(Vec::new());
    assert!(matches!(sessions, ServerMsg::Sessions(items) if items.is_empty()));
}
