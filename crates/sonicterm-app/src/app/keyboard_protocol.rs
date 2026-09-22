use super::key_encoding::{encode_win32_key, Win32KeyEvent};
use sonicterm_vt::vt::KeyboardModes;
use std::collections::{BTreeMap, HashMap};
use winit::keyboard::PhysicalKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyboardProtocol {
    Other,
    Win32,
    Unavailable,
}

#[derive(Clone, Copy)]
pub(super) struct KeyboardSnapshot(u64);

impl KeyboardSnapshot {
    /// Decode one coherent parser publication without loading independent flag locations.
    pub(super) fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Return the packed terminal keyboard modes.
    pub(super) fn modes(self) -> KeyboardModes {
        KeyboardModes::from_bits(self.0 as u8)
    }

    /// Return every negotiated Kitty flag, including unknown bits.
    pub(super) fn kitty_flags(self) -> u8 {
        (self.0 >> 8) as u8
    }

    fn epoch(self) -> u64 {
        self.0 >> 16
    }

    /// Describe unavailable native input without exposing key data or warning on synthetic focus events.
    pub(super) fn refusal_reason(
        self,
        windows: bool,
        has_native: bool,
        synthetic: bool,
    ) -> Option<&'static str> {
        if synthetic {
            // When: synthetic focus input is ignored intentionally, it is not a metadata failure.
            return None;
        }
        match self.protocol(windows) {
            KeyboardProtocol::Unavailable => Some("protocol epoch exhausted"),
            KeyboardProtocol::Win32 if !has_native => Some("native metadata unavailable"),
            _ => None,
        }
    }

    /// Select native input only on Windows when no Kitty flag takes precedence.
    pub(super) fn protocol(self, windows: bool) -> KeyboardProtocol {
        if !windows || !self.modes().win32_input() || self.kitty_flags() != 0 {
            // When: windows, win32_input, or kitty_flags excludes native input, preserve the existing encoder.
            return KeyboardProtocol::Other;
        }
        if self.epoch() == (1 << 48) - 1 {
            // When: epoch is saturated, no reusable native identity can safely distinguish future transitions.
            return KeyboardProtocol::Unavailable;
        }
        KeyboardProtocol::Win32
    }
}

/// Accepted press ownership, with native identity retained without character data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldKey {
    /// Legacy and Kitty input follows live non-native flags.
    Legacy,
    /// Native input belongs to the protocol epoch that accepted the press.
    Win32 { epoch: u64, virtual_key: u16, scan_code: u16, control_key_state: u32 },
}

impl HeldKey {
    /// Test whether the current snapshot still owns this accepted press.
    pub(super) fn compatible(self, state: KeyboardSnapshot, windows: bool) -> bool {
        match self {
            Self::Legacy => state.protocol(windows) == KeyboardProtocol::Other,
            Self::Win32 { epoch, .. } => {
                state.protocol(windows) == KeyboardProtocol::Win32 && state.epoch() == epoch
            }
        }
    }

    /// Encode focus-loss cleanup using last-known lock flags, with every held modifier cleared.
    pub(super) fn focus_release(self, state: KeyboardSnapshot, windows: bool) -> Option<Vec<u8>> {
        if !self.compatible(state, windows) {
            // When: compatible rejects state, cleanup must not cross protocol families.
            return None;
        }
        let Self::Win32 { virtual_key, scan_code, control_key_state, .. } = self else {
            // When: Self::Win32 does not match, focus loss preserves the existing no-cleanup behavior.
            return None;
        };
        Some(encode_win32_key(Win32KeyEvent {
            virtual_key,
            scan_code,
            unicode: &[],
            key_down: false,
            control_key_state: control_key_state & (32 | 64 | 128 | 256),
            repeat_count: 1,
        }))
    }
}

pub(super) type KeyRoutes = BTreeMap<u64, HeldKey>;
pub(super) type PressedKeys = HashMap<PhysicalKey, KeyRoutes>;

pub(super) struct EncodedKey {
    pub bytes: Vec<u8>,
    pub held: HeldKey,
}

/// Encode from one snapshot, refusing unmatched native repeats, releases, or synthetic input.
pub(super) fn encode_routed_key(
    state: KeyboardSnapshot,
    windows: bool,
    native: Option<Win32KeyEvent<'_>>,
    synthetic: bool,
    repeat: bool,
    previous: Option<HeldKey>,
    other: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<EncodedKey> {
    if previous.is_some_and(|held| !held.compatible(state, windows))
        || (repeat && previous.is_none())
    {
        // When: ownership is absent or incompatible, never migrate a held event to a new protocol.
        return None;
    }
    match state.protocol(windows) {
        KeyboardProtocol::Unavailable => None,
        KeyboardProtocol::Other => other()
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| EncodedKey { bytes, held: HeldKey::Legacy }),
        KeyboardProtocol::Win32 => {
            // When: KeyboardProtocol::Win32 owns input, only native records can continue an accepted hold.
            if synthetic {
                // When: synthetic is true, explicit focus cleanup owns native releases.
                return None;
            }
            let native = native?;
            if !native.key_down && previous.is_none() {
                // When: a release has no accepted native press, it cannot create ownership.
                return None;
            }
            if let Some(HeldKey::Win32 { scan_code, .. }) = previous {
                // When: previous is Win32, the physical route and scan survive VK changes caused by NumLock or layout state.
                if native.scan_code != scan_code {
                    // When: scan_code differs, the native record cannot continue this physical hold.
                    return None;
                }
            }
            let held = previous.unwrap_or(HeldKey::Win32 {
                epoch: state.epoch(),
                virtual_key: native.virtual_key,
                scan_code: native.scan_code,
                control_key_state: native.control_key_state,
            });
            Some(EncodedKey { bytes: encode_win32_key(native), held })
        }
    }
}

/// Retain routes only after their encoded press enters the target's bounded input queue.
pub(super) fn dispatch_key_writes(
    writes: Vec<(u64, EncodedKey)>,
    mut send: impl FnMut(u64, Vec<u8>) -> bool,
) -> KeyRoutes {
    writes
        .into_iter()
        .filter_map(|(pane, encoded)| send(pane, encoded.bytes).then_some((pane, encoded.held)))
        .collect()
}

/// Leave synthetic native releases pending for focus cleanup while releasing other routes normally.
pub(super) fn take_release_routes(
    pressed: &mut PressedKeys,
    key: PhysicalKey,
    synthetic: bool,
) -> Option<KeyRoutes> {
    let mut routes = pressed.remove(&key)?;
    if synthetic {
        // Native ownership survives synthetic releases until the explicit focus-loss drain.
        let retained: KeyRoutes = routes
            .iter()
            .filter(|(_, held)| matches!(held, HeldKey::Win32 { .. }))
            .map(|(pane, held)| (*pane, *held))
            .collect();
        routes.retain(|_, held| matches!(held, HeldKey::Legacy));
        if !retained.is_empty() {
            pressed.insert(key, retained);
        }
    }
    Some(routes)
}

#[cfg(test)]
#[path = "keyboard_protocol_tests.rs"]
mod keyboard_protocol_tests;
