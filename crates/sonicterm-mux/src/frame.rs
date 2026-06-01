//! Length-prefixed bincode framing over any `Read`/`Write` stream.

use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Serialize};

/// Maximum allowed frame size: 16 MiB. Protects against accidental or
/// malicious unbounded allocations on a corrupted stream.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Serialize `msg` with bincode and write it as a 4-byte big-endian
/// length prefix followed by the payload, then flush the stream.
pub fn write_frame<W: Write, M: Serialize>(w: &mut W, msg: &M) -> io::Result<()> {
    let bytes = bincode::serialize(msg).map_err(|e| io::Error::other(e.to_string()))?;
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
    bincode::deserialize(&buf).map_err(|e| io::Error::other(e.to_string()))
}
