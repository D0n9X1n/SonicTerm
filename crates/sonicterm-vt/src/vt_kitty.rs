//! Kitty graphics command keys: the subset SonicTerm accepts, and the reply format.
//!
//! A kitty graphics APC is `ESC _ G <key=value,...> [; <base64 payload>] ESC \`. This module turns
//! the control section into a typed [`KittyCommand`] and formats the `ESC _ G i=...;OK ESC \`
//! replies. Staging the payload and reassembling chunks belongs to the stream layer; decoding
//! pixels and placing them belongs to the host.

/// What a kitty graphics command asks for (`a=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KittyAction {
    /// `a=t`: store the image without displaying it. The protocol default.
    #[default]
    Transmit,
    /// `a=T`: store the image and display it.
    TransmitAndDisplay,
    /// `a=q`: check that the image would load, storing nothing.
    Query,
    /// `a=p`: display an image transmitted earlier.
    Place,
    /// `a=d`: delete images or placements.
    Delete,
}

/// How the payload reaches the terminal (`t=`).
///
/// Only direct transmission is accepted. The other media name a file, temporary file or shared
/// memory object, so a program in the pane — including one on a remote host over SSH — could
/// make the terminal read local data it chose. They are parsed only to be refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KittyMedium {
    /// `t=d`: the payload is the image data. The protocol default.
    #[default]
    Direct,
    /// `t=f`, `t=t` or `t=s`: refused.
    Refused,
}

/// Which images a delete command removes (`d=`), for the selectors this subset supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KittyDelete {
    /// `d=i`: remove the image's placements, keeping its data.
    ById,
    /// `d=I`: remove the image's placements and free its data once nothing places it.
    ByIdFree,
    /// Any other selector, ignored by this subset.
    Unsupported,
}

/// A parsed kitty graphics control section.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct KittyCommand {
    /// The requested action.
    pub action: KittyAction,
    /// The transmission medium.
    pub medium: KittyMedium,
    /// Pixel format: 24 (RGB), 32 (RGBA) or 100 (PNG). Defaults to 32.
    pub format: u32,
    /// Whether more chunks follow (`m=1`).
    pub more: bool,
    /// Reply suppression: 0 all replies, 1 errors only, 2 none.
    pub quiet: u8,
    /// Whether `q` was given, so a later chunk's explicit `q=0` can restore replies.
    pub quiet_set: bool,
    /// Client image id (`i=`); 0 when absent.
    pub image_id: u32,
    /// Placement id (`p=`); 0 when absent.
    pub placement_id: u32,
    /// Raw image width in pixels (`s=`), for formats 24 and 32.
    pub width_px: u32,
    /// Raw image height in pixels (`v=`), for formats 24 and 32.
    pub height_px: u32,
    /// Placement width in cells (`c=`); 0 when absent.
    pub cols: u32,
    /// Placement height in cells (`r=`); 0 when absent.
    pub rows: u32,
    /// Virtual placement drawn through Unicode placeholders (`U=1`).
    pub virtual_placement: bool,
    /// Delete selector, for `a=d`. `None` defaults to kitty's `a`, which is unsupported here.
    pub delete: Option<KittyDelete>,
    /// Whether the command carried only `m` and `q` keys, so it can continue an open transfer.
    pub continuation_only: bool,
}

/// Why a control section was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KittyCommandError {
    /// A key had a value outside its domain, or an image number (`I=`) was given.
    Invalid,
    /// A value this subset does not implement: compression (`o=`) or an action outside `t,T,q,p,d`.
    Unsupported,
}

impl KittyCommandError {
    /// The kitty error code and message carried in a reply.
    #[must_use]
    pub fn reply_message(self) -> &'static str {
        match self {
            Self::Invalid => "EINVAL:invalid command",
            Self::Unsupported => "ENOTSUPPORTED:unsupported command",
        }
    }
}

/// The value of a numeric key, or an invalid-command error.
fn number(value: &[u8]) -> Result<u32, KittyCommandError> {
    if value.is_empty() || value.len() > 10 || !value.iter().all(u8::is_ascii_digit) {
        // When: value is empty, too long for u32 or not all digits, it is outside every numeric key's domain.
        return Err(KittyCommandError::Invalid);
    }
    std::str::from_utf8(value)
        .ok()
        .and_then(|digits| digits.parse().ok())
        .ok_or(KittyCommandError::Invalid)
}

/// The value of a small enumerated key, `0..=max`, or an invalid-command error.
fn flag(value: &[u8], max: u8) -> Result<u8, KittyCommandError> {
    let parsed = number(value)?;
    u8::try_from(parsed).ok().filter(|flag| *flag <= max).ok_or(KittyCommandError::Invalid)
}

impl KittyCommand {
    /// Parse a control section: the bytes after `G` and before `;` or the terminator.
    ///
    /// Unknown keys are ignored, as kitty does, so a newer client's extra keys do not break it.
    /// The transmission medium is parsed even when refused, so the caller can answer it and
    /// swallow the payload.
    pub fn parse(control: &[u8]) -> Result<Self, KittyCommandError> {
        let mut command = Self { format: 32, continuation_only: true, ..Self::default() };
        let mut saw_key = false;
        for pair in control.split(|byte| *byte == b',').filter(|pair| !pair.is_empty()) {
            let Some((key, value)) = super::split_once_byte(pair, b'=') else {
                // When: pair has no `=`, it is not a key=value item.
                return Err(KittyCommandError::Invalid);
            };
            saw_key = true;
            // Any key other than m or q means this chunk starts a command rather than continuing one.
            if !matches!(key, b"m" | b"q") {
                command.continuation_only = false;
            }
            match key {
                b"a" => {
                    // When: key is a, value selects the action; animation and composition actions are refused.
                    command.action = match value {
                        b"t" => KittyAction::Transmit,
                        b"T" => KittyAction::TransmitAndDisplay,
                        b"q" => KittyAction::Query,
                        b"p" => KittyAction::Place,
                        b"d" => KittyAction::Delete,
                        _ => {
                            // When: value names an animation or composition action, this subset does not implement it.
                            return Err(KittyCommandError::Unsupported);
                        }
                    }
                }
                b"t" => {
                    // When: key is t, value selects the medium; only direct data is ever read.
                    command.medium = match value {
                        b"d" => KittyMedium::Direct,
                        b"f" | b"t" | b"s" => KittyMedium::Refused,
                        _ => {
                            // When: value is no medium kitty defines, the command is malformed.
                            return Err(KittyCommandError::Invalid);
                        }
                    }
                }
                b"f" => {
                    // When: key is f, value must name RGB (24), RGBA (32) or PNG (100).
                    command.format = number(value)?;
                    if !matches!(command.format, 24 | 32 | 100) {
                        // When: the format number matches none of 24, 32 or 100, nothing here can decode it.
                        return Err(KittyCommandError::Invalid);
                    }
                }
                b"o" => {
                    // When: key is o, the payload is compressed, which this subset cannot inflate.
                    return Err(KittyCommandError::Unsupported);
                }
                b"m" => command.more = flag(value, 1)? == 1,
                b"q" => {
                    // The value is recorded and marked explicit, so chunk merging can tell q=0 from no q.
                    command.quiet = flag(value, 2)?;
                    command.quiet_set = true;
                }
                b"i" => command.image_id = number(value)?,
                b"I" => {
                    // When: key is I, the client asks for an image number, which is refused rather than aliased to i.
                    return Err(KittyCommandError::Invalid);
                }
                b"p" => command.placement_id = number(value)?,
                b"s" => command.width_px = number(value)?,
                b"v" => command.height_px = number(value)?,
                b"c" => command.cols = number(value)?,
                b"r" => command.rows = number(value)?,
                b"U" => command.virtual_placement = flag(value, 1)? == 1,
                b"d" => {
                    command.delete = Some(match value {
                        b"i" => KittyDelete::ById,
                        b"I" => KittyDelete::ByIdFree,
                        _ => KittyDelete::Unsupported,
                    })
                }
                _ => {
                    // When: key is not one this subset reads, it is ignored as kitty ignores unknown keys.
                }
            }
        }
        command.continuation_only &= saw_key;
        Ok(command)
    }

    /// Whether this command's payload is image data the stream must stage.
    #[must_use]
    pub fn carries_image(&self) -> bool {
        matches!(
            self.action,
            KittyAction::Transmit | KittyAction::TransmitAndDisplay | KittyAction::Query
        )
    }

    /// A command carrying only the image id and quiet level a malformed control section names,
    /// so the client can still be told it was rejected. Keys that do not parse are skipped.
    #[must_use]
    pub fn reply_target(control: &[u8]) -> Self {
        let mut target = Self::default();
        for pair in control.split(|byte| *byte == b',') {
            match super::split_once_byte(pair, b'=') {
                Some((b"i", value)) => target.image_id = number(value).unwrap_or(0),
                Some((b"q", value)) => target.quiet = flag(value, 2).unwrap_or(0),
                _ => {
                    // When: the pair is any other key or malformed, it does not address the reply.
                }
            }
        }
        target
    }

    /// The reply to `message` for this command, or `None` when quiet mode or a missing image id
    /// suppresses it.
    ///
    /// Kitty answers only commands that name an image id. `q=1` suppresses `OK`; `q=2` suppresses
    /// every reply.
    #[must_use]
    pub fn reply(&self, message: &str) -> Option<Vec<u8>> {
        let is_ok = message == "OK";
        if self.image_id == 0 || self.quiet >= 2 || (self.quiet == 1 && is_ok) {
            // When: image_id is 0, quiet is 2, or quiet is 1 and is_ok, kitty sends nothing back.
            return None;
        }
        let mut reply = format!("\x1b_Gi={}", self.image_id);
        // A set placement id is echoed so the client can match the reply.
        if self.placement_id != 0 {
            reply.push_str(&format!(",p={}", self.placement_id));
        }
        reply.push(';');
        reply.push_str(message);
        reply.push_str("\x1b\\");
        Some(reply.into_bytes())
    }
}

#[cfg(test)]
#[path = "vt_kitty_tests.rs"]
mod vt_kitty_tests;
