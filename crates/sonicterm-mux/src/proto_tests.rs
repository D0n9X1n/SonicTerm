use super::*;
use serde::{de::DeserializeOwned, Serialize};
use std::fmt::Debug;

fn round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + Debug,
{
    let bytes = bincode::serde::encode_to_vec(value, bincode::config::standard()).unwrap();
    let (decoded, consumed): (T, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(format!("{decoded:?}"), format!("{value:?}"));
}

#[test]
fn every_client_message_round_trips() {
    let messages = vec![
        ClientMsg::ListSessions,
        ClientMsg::Attach(42),
        ClientMsg::Detach,
        ClientMsg::Spawn { cmd: "/bin/zsh".into(), cols: 120, rows: 40 },
        ClientMsg::Input { pane_id: 7, bytes: vec![0, 1, 255] },
        ClientMsg::Resize { pane_id: 7, cols: 80, rows: 24 },
        ClientMsg::Kill { pane_id: 7 },
    ];
    for message in messages {
        round_trip(&message);
    }
}

#[test]
fn every_server_message_round_trips() {
    let pane = PaneInfo { id: 7, cmd: "/bin/zsh".into(), cols: 120, rows: 40 };
    let messages = vec![
        ServerMsg::Sessions(vec![SessionInfo { id: 42, pane_count: 1 }]),
        ServerMsg::AttachOk { session_id: 42, panes: vec![pane] },
        ServerMsg::Spawned { session_id: 42, pane_id: 7 },
        ServerMsg::Output { pane_id: 7, bytes: vec![0, 1, 255] },
        ServerMsg::Exit { pane_id: 7 },
        ServerMsg::Error("bad request".into()),
    ];
    for message in messages {
        round_trip(&message);
    }
}
