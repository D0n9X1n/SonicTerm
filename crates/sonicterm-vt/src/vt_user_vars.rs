//! OSC 1337 `SetUserVar`: per-pane user variables a program sets, bounded and strictly decoded.
//!
//! The raw collector owns the whole OSC (at most [`MAX_USER_VAR_OSC_BYTES`]) and hands it here
//! only once complete. Every `SetUserVar=` record applies in order: `NAME=B64` sets, `NAME=` sets
//! an empty value, `NAME` alone deletes. A bad record is dropped alone. Names and values are
//! program-supplied display metadata: never logged, only counted.

use std::sync::Arc;

use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;

/// The largest whole OSC 1337 `SetUserVar` payload the raw collector owns, in wire bytes.
pub(super) const MAX_USER_VAR_OSC_BYTES: usize = 64 * 1024;
/// The most variables one pane holds.
pub(super) const MAX_USER_VARS: usize = 64;
/// The most name and value bytes one pane holds in all.
pub(super) const MAX_USER_VAR_STORE_BYTES: usize = 16 * 1024;
/// The longest name, in bytes.
pub(super) const MAX_USER_VAR_NAME_BYTES: usize = 128;
/// The longest decoded value, in bytes.
pub(super) const MAX_USER_VAR_VALUE_BYTES: usize = 4 * 1024;
/// The record key the collector owns, and each record's prefix.
pub(super) const SET_USER_VAR_KEY: &[u8] = b"SetUserVar=";
/// The most heap one pane's variables own: an entry vector that doubles up to twice
/// [`MAX_USER_VARS`] slots, and strings collected by character, which may double their content.
pub(super) const MAX_USER_VARS_BYTES: usize =
    2 * MAX_USER_VARS * std::mem::size_of::<(String, String)>() + 2 * MAX_USER_VAR_STORE_BYTES;

/// Standard base64 with optional padding, as WezTerm and kitty accept.
const VALUE_ENGINE: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// One pane's user variables, oldest first. Program-supplied display metadata: never logged.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct UserVars {
    /// Each name and its value, in the order they were last set.
    entries: Vec<(String, String)>,
    /// The name and value bytes of every entry, at most [`MAX_USER_VAR_STORE_BYTES`].
    held_bytes: usize,
}

impl UserVars {
    /// Each name and value, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// How many variables are held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no variable is held.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Heap bytes the store owns: its entry vector's capacity and every name's and value's
    /// capacity, its share of [`super::MAX_PROGRAM_METADATA_BYTES`]. Persistent pane state, never
    /// capture.
    pub fn retained_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<(String, String)>()
            + self
                .entries
                .iter()
                .map(|(name, value)| name.capacity() + value.capacity())
                .sum::<usize>()
    }

    /// Set `name` to `value` as the newest entry, evicting the oldest until both caps hold.
    fn set(&mut self, name: String, value: String) {
        self.remove(&name);
        let added_bytes = name.len() + value.len();
        // Each record is at most 128 + 4 KiB bytes, below the 16 KiB budget, so eviction ends.
        while self.entries.len() >= MAX_USER_VARS
            || self.held_bytes + added_bytes > MAX_USER_VAR_STORE_BYTES
        {
            let (old_name, old_value) = self.entries.remove(0);
            self.held_bytes -= old_name.len() + old_value.len();
        }
        self.held_bytes += added_bytes;
        self.entries.push((name, value));
    }

    /// Delete `name` when held.
    fn remove(&mut self, name: &str) {
        if let Some(index) = self.entries.iter().position(|(held, _)| held == name) {
            let (old_name, old_value) = self.entries.remove(index);
            self.held_bytes -= old_name.len() + old_value.len();
        }
    }
}

impl std::fmt::Debug for UserVars {
    /// Counts only: a name or value printed in a log line would leak program-supplied text.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserVars")
            .field("count", &self.entries.len())
            .field("bytes", &self.held_bytes)
            .finish()
    }
}

/// What one valid record asks for.
enum UserVarRecord {
    /// Set the name to the value.
    Set(String, String),
    /// Delete the name.
    Delete(String),
}

/// Apply every `SetUserVar=` record of one complete OSC 1337 `payload` to `store`, in order.
/// `store` is copied only when a reader still holds the previous snapshot.
pub(super) fn apply_payload(store: &mut Arc<UserVars>, payload: &[u8]) {
    let mut dropped_count = 0usize;
    let mut first_reason = None;
    for record in payload.split(|byte| *byte == b';') {
        let Some(body) = record.strip_prefix(SET_USER_VAR_KEY) else {
            // When: the record is not `SetUserVar=` (`CurrentDir=`, padding), it is skipped.
            continue;
        };
        match parse_record(body) {
            Ok(UserVarRecord::Set(name, value)) => Arc::make_mut(store).set(name, value),
            Ok(UserVarRecord::Delete(name)) => Arc::make_mut(store).remove(&name),
            Err(reason) => {
                dropped_count += 1;
                first_reason.get_or_insert(reason);
            }
        }
    }
    if let Some(reason) = first_reason {
        // Counts and a reason code only: names and values are never logged.
        tracing::debug!(
            target: "vt",
            dropped_count,
            payload_bytes = payload.len(),
            reason,
            "OSC 1337 SetUserVar records dropped"
        );
    }
}

/// Read one record's `body` (after `SetUserVar=`), or the reason code it is dropped for.
fn parse_record(body: &[u8]) -> Result<UserVarRecord, &'static str> {
    let (name, encoded) = match body.iter().position(|byte| *byte == b'=') {
        Some(split) => (&body[..split], Some(&body[split + 1..])),
        None => (body, None),
    };
    if name.is_empty() || name.len() > MAX_USER_VAR_NAME_BYTES {
        // When: `name` is empty or past its cap, the record is dropped whole.
        return Err("name_length");
    }
    let name = sanitized_text(name).ok_or("name_utf8")?;
    let Some(encoded) = encoded else {
        // When: `encoded` is absent because the record has no second `=`, it deletes the name.
        return Ok(UserVarRecord::Delete(name));
    };
    // Base64 needs 4 characters per 3 bytes, so a longer encoding cannot decode within the cap.
    if encoded.len() > MAX_USER_VAR_VALUE_BYTES.div_ceil(3) * 4 {
        // When: `encoded` is longer than the largest value's encoding, it is dropped undecoded.
        return Err("value_length");
    }
    let decoded = VALUE_ENGINE.decode(encoded).map_err(|_| "value_base64")?;
    if decoded.len() > MAX_USER_VAR_VALUE_BYTES {
        // When: `decoded` is past the value cap, the record is dropped whole, never cut.
        return Err("value_length");
    }
    let value = sanitized_text(&decoded).ok_or("value_utf8")?;
    Ok(UserVarRecord::Set(name, value))
}

/// `bytes` as strict UTF-8 with C0, DEL and C1 controls shown as spaces, or `None` when it is not
/// UTF-8. Each control becomes a one-byte space, so the text never grows past the cap it met.
fn sanitized_text(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    Some(
        text.chars()
            .map(|character| if character.is_control() { ' ' } else { character })
            .collect(),
    )
}

#[cfg(test)]
#[path = "vt_user_vars_tests.rs"]
mod vt_user_vars_tests;
