//! Length-prefixed bincode framing over any `Read`/`Write` stream.

use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Serialize};

/// Maximum allowed frame size: 16 MiB. Protects against accidental or
/// malicious unbounded allocations on a corrupted stream.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Serialize `msg` with bincode and write it as a 4-byte big-endian
/// length prefix followed by the payload, then flush the stream.
pub fn write_frame<W: Write, M: Serialize>(w: &mut W, msg: &M) -> io::Result<()> {
    let bytes = bincode::serde::encode_to_vec(msg, bincode::config::standard())
        .map_err(|e| io::Error::other(e.to_string()))?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::other(format!("frame too large: {}", bytes.len())));
    }
    let len = (bytes.len() as u32).to_be_bytes();
    w.write_all(&len)?;
    w.write_all(&bytes)?;
    w.flush()?;
    Ok(())
}

/// Read a 4-byte big-endian length prefix and the following payload, then
/// deserialize it with bincode. Returns an error if the declared length
/// exceeds `MAX_FRAME` so a corrupt stream can't trigger a huge allocation.
pub fn read_frame<R: Read, M: DeserializeOwned>(r: &mut R) -> io::Result<M> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::other(format!("frame too large: {len}")));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    bincode::serde::decode_from_slice(&buf, bincode::config::standard())
        .map(|(msg, _)| msg)
        .map_err(|e| io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
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
}

