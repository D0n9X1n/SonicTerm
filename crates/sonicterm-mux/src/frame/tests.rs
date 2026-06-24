use super::*;
use crate::proto::ClientMsg;
use std::io::Cursor;

#[test]
fn round_trips_a_protocol_message() {
    let msg = ClientMsg::Spawn {
        cmd: "/bin/zsh".to_string(),
        cols: 120,
        rows: 40,
    };

    let mut buf = Vec::new();
    write_frame(&mut buf, &msg).expect("write_frame");

    // 4-byte big-endian length prefix + payload.
    assert!(buf.len() > 4);
    let declared = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
    assert_eq!(declared, buf.len() - 4);

    let mut cur = Cursor::new(buf);
    let decoded: ClientMsg = read_frame(&mut cur).expect("read_frame");
    // ClientMsg intentionally does not derive PartialEq (wire type), so
    // compare via Debug to keep this test scoped to the framing change.
    assert_eq!(format!("{decoded:?}"), format!("{msg:?}"));
}

#[test]
fn rejects_oversized_declared_length() {
    // Length prefix claims more than MAX_FRAME with no payload following.
    let len = (MAX_FRAME as u32 + 1).to_be_bytes();
    let mut cur = Cursor::new(len.to_vec());
    let err = read_frame::<_, ClientMsg>(&mut cur).expect_err("must reject");
    assert!(err.to_string().contains("frame too large"));
}

