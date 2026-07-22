use std::{io::Cursor, sync::Arc};

use sonicterm_mux::{handle_connection, ServerState};

#[test]
fn legacy_handle_connection_signature_remains_public() {
    let legacy: fn(
        Arc<ServerState>,
        Cursor<Vec<u8>>,
        Cursor<Vec<u8>>,
    ) -> anyhow::Result<()> = handle_connection::<Cursor<Vec<u8>>>;

    let _ = legacy;
}
