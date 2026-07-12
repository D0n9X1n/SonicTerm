use super::*;
use crate::proto::ClientMsg;
use std::io::{Cursor, ErrorKind};

fn payload(message: &ClientMsg) -> Vec<u8> {
    bincode::serde::encode_to_vec(message, bincode::config::standard()).unwrap()
}

#[test]
fn round_trips_a_protocol_message() {
    let msg = ClientMsg::Spawn { cmd: "/bin/zsh".to_string(), cols: 120, rows: 40 };
    let mut buf = Vec::new();
    write_frame(&mut buf, &msg).unwrap();
    assert_eq!(u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize, buf.len() - 4);
    let decoded: ClientMsg = read_frame(&mut Cursor::new(buf)).unwrap();
    assert_eq!(format!("{decoded:?}"), format!("{msg:?}"));
}

#[test]
fn concatenated_frames_are_consumed_one_at_a_time() {
    let first = ClientMsg::Attach(42);
    let second = ClientMsg::Kill { pane_id: 7 };
    let mut buf = Vec::new();
    write_frame(&mut buf, &first).unwrap();
    write_frame(&mut buf, &second).unwrap();
    let mut cursor = Cursor::new(buf);
    let got_first: ClientMsg = read_frame(&mut cursor).unwrap();
    let got_second: ClientMsg = read_frame(&mut cursor).unwrap();
    assert_eq!(format!("{got_first:?}"), format!("{first:?}"));
    assert_eq!(format!("{got_second:?}"), format!("{second:?}"));
}

#[test]
fn rejects_oversized_declared_length() {
    let mut cur = Cursor::new((MAX_FRAME as u32 + 1).to_be_bytes().to_vec());
    assert!(read_frame::<_, ClientMsg>(&mut cur)
        .unwrap_err()
        .to_string()
        .contains("frame too large"));
}

#[test]
fn rejects_short_prefix_and_short_payload() {
    let err = read_frame::<_, ClientMsg>(&mut Cursor::new(vec![0, 0, 0])).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnexpectedEof);

    let mut bytes = 10u32.to_be_bytes().to_vec();
    bytes.extend([0, 1]);
    let err = read_frame::<_, ClientMsg>(&mut Cursor::new(bytes)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
}

#[test]
fn rejects_empty_malformed_and_intra_frame_trailing_payload() {
    assert!(read_frame::<_, ClientMsg>(&mut Cursor::new(0u32.to_be_bytes().to_vec())).is_err());

    let mut malformed = 3u32.to_be_bytes().to_vec();
    malformed.extend([0xff, 0xff, 0xff]);
    assert!(read_frame::<_, ClientMsg>(&mut Cursor::new(malformed)).is_err());

    let encoded = payload(&ClientMsg::Detach);
    let mut padded = ((encoded.len() + 2) as u32).to_be_bytes().to_vec();
    padded.extend(encoded);
    padded.extend([0, 0]);
    let err = read_frame::<_, ClientMsg>(&mut Cursor::new(padded)).unwrap_err();
    assert!(err.to_string().contains("trailing bytes"));
}
