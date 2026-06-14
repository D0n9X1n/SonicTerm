//! Keyboard event → byte encoding for the PTY side, plus
//! `KeyName` mapping used by the keymap parser.
//!
//! Extracted from `app/mod.rs` in refactor PR 8b.

use sonicterm_cfg::keymap::ScrollAction;
use winit::{
    event::KeyEvent,
    keyboard::{Key, ModifiersState, NamedKey},
};

pub(super) fn _scroll_used(_a: ScrollAction) {}

pub(super) fn encode_key(
    event: &KeyEvent,
    mods: ModifiersState,
    kitty_flags: u8,
) -> Option<Vec<u8>> {
    encode_logical(&event.logical_key, mods, kitty_flags)
}

/// Encode a logical key + modifiers into the bytes to send to the PTY.
///
/// `kitty_flags` is the active kitty keyboard protocol flag set reported by
/// the focused pane's parser (see [`sonicterm_vt`]). `0` means no kitty
/// progressive enhancement is active, so we emit legacy encodings. When a
/// modern TUI (Copilot CLI, claude, etc.) has pushed a non-zero flag set, we
/// encode the keys that matter for multi-line input in CSI-u form — at minimum
/// Shift+Enter as `CSI 13 ; 2 u` so the app inserts a newline instead of
/// submitting. Everything else falls through to the legacy encoding.
#[doc(hidden)]
pub fn encode_logical(key: &Key, mods: ModifiersState, kitty_flags: u8) -> Option<Vec<u8>> {
    let ctrl = mods.control_key();
    match key {
        Key::Named(n) => Some(match n {
            // Shift+Enter: under the kitty keyboard protocol, encode the
            // disambiguated CSI-u form (codepoint 13 = Return, modifier 2 =
            // 1 + Shift bit). Copilot CLI / claude treat this as "insert
            // newline" rather than "submit". With no kitty flags active we
            // fall back to the WezTerm-default ESC+CR, which most apps map to
            // the same intent and is harmless for those that don't.
            NamedKey::Enter if mods.shift_key() => {
                if kitty_flags != 0 {
                    b"\x1b[13;2u".to_vec()
                } else {
                    b"\x1b\r".to_vec()
                }
            }
            NamedKey::Enter => b"\r".to_vec(),
            NamedKey::Backspace => b"\x7f".to_vec(),
            NamedKey::Tab => b"\t".to_vec(),
            NamedKey::Escape => b"\x1b".to_vec(),
            NamedKey::Space => b" ".to_vec(),
            NamedKey::ArrowUp => b"\x1b[A".to_vec(),
            NamedKey::ArrowDown => b"\x1b[B".to_vec(),
            NamedKey::ArrowRight => b"\x1b[C".to_vec(),
            NamedKey::ArrowLeft => b"\x1b[D".to_vec(),
            NamedKey::Home => b"\x1b[H".to_vec(),
            NamedKey::End => b"\x1b[F".to_vec(),
            NamedKey::PageUp => b"\x1b[5~".to_vec(),
            NamedKey::PageDown => b"\x1b[6~".to_vec(),
            NamedKey::Delete => b"\x1b[3~".to_vec(),
            NamedKey::F1 => encode_function_key(1, mods),
            NamedKey::F2 => encode_function_key(2, mods),
            NamedKey::F3 => encode_function_key(3, mods),
            NamedKey::F4 => encode_function_key(4, mods),
            NamedKey::F5 => encode_function_key(5, mods),
            NamedKey::F6 => encode_function_key(6, mods),
            NamedKey::F7 => encode_function_key(7, mods),
            NamedKey::F8 => encode_function_key(8, mods),
            NamedKey::F9 => encode_function_key(9, mods),
            NamedKey::F10 => encode_function_key(10, mods),
            NamedKey::F11 => encode_function_key(11, mods),
            NamedKey::F12 => encode_function_key(12, mods),
            _ => return None,
        }),
        Key::Character(s) => {
            if ctrl {
                let mut bytes = Vec::with_capacity(1);
                for ch in s.chars() {
                    let lower = ch.to_ascii_lowercase();
                    if lower.is_ascii_lowercase() {
                        bytes.push((lower as u8) - b'a' + 1);
                    } else {
                        bytes.extend(ch.to_string().as_bytes());
                    }
                }
                Some(bytes)
            } else {
                Some(s.as_bytes().to_vec())
            }
        }
        _ => None,
    }
}

/// Encode a function key F1–F12 with xterm-style modifier handling.
///
/// `n` is the function-key number (1..=12). With no modifiers, F1–F4 use the
/// legacy SS3 forms (`ESC O P`..`ESC O S`) and F5–F12 use the CSI tilde forms
/// (`CSI 15~`, `CSI 17~`, …). When any modifier is held, every key switches to
/// the parameterized CSI form xterm defines:
///
/// - F1–F4 → `CSI 1 ; <mod> [P|Q|R|S]`
/// - F5–F12 → `CSI <code> ; <mod> ~`
///
/// `<mod>` is `1 + bitmask`, where Shift=1, Alt=2, Ctrl=4, Super/Meta=8 — the
/// same convention xterm, WezTerm, and tmux speak, so apps like nvim decode the
/// chord correctly.
fn encode_function_key(n: u8, mods: ModifiersState) -> Vec<u8> {
    // SS3 final byte for F1–F4, used only in the unmodified legacy form.
    let ss3_final = |n: u8| match n {
        1 => b'P',
        2 => b'Q',
        3 => b'R',
        _ => b'S', // n == 4
    };
    // CSI tilde code for F5–F12 (the gaps at 16/22 are historical xterm).
    let tilde_code = |n: u8| -> u8 {
        match n {
            5 => 15,
            6 => 17,
            7 => 18,
            8 => 19,
            9 => 20,
            10 => 21,
            11 => 23,
            _ => 24, // n == 12
        }
    };

    let modifier_param = function_key_modifier_param(mods);

    match (n, modifier_param) {
        // Unmodified F1–F4: legacy SS3.
        (1..=4, None) => vec![0x1b, b'O', ss3_final(n)],
        // Modified F1–F4: CSI 1 ; <mod> <final>.
        (1..=4, Some(m)) => {
            let mut out = format!("\x1b[1;{m}").into_bytes();
            out.push(ss3_final(n));
            out
        }
        // Unmodified F5–F12: CSI <code> ~.
        (_, None) => format!("\x1b[{}~", tilde_code(n)).into_bytes(),
        // Modified F5–F12: CSI <code> ; <mod> ~.
        (_, Some(m)) => format!("\x1b[{};{m}~", tilde_code(n)).into_bytes(),
    }
}

/// xterm modifier parameter (`1 + bitmask`) for a function-key chord, or `None`
/// when no modifier is held. Bits: Shift=1, Alt=2, Ctrl=4, Super/Meta=8.
fn function_key_modifier_param(mods: ModifiersState) -> Option<u8> {
    let mut bitmask = 0u8;
    if mods.shift_key() {
        bitmask |= 1;
    }
    if mods.alt_key() {
        bitmask |= 2;
    }
    if mods.control_key() {
        bitmask |= 4;
    }
    if mods.super_key() {
        bitmask |= 8;
    }
    if bitmask == 0 {
        None
    } else {
        Some(bitmask + 1)
    }
}

pub(super) fn key_event_to_string(event: &KeyEvent, mods: ModifiersState) -> Option<String> {
    key_to_string(&event.logical_key, mods)
}

#[doc(hidden)]
pub fn key_to_string(key: &Key, mods: ModifiersState) -> Option<String> {
    let mut candidates = key_candidates(key)?;
    candidates.dedup();
    let candidate = candidates.into_iter().next()?;
    Some(chord_string(candidate.as_str(), mods))
}

#[doc(hidden)]
pub fn key_to_strings(key: &Key, mods: ModifiersState) -> Vec<String> {
    let Some(mut candidates) = key_candidates(key) else { return Vec::new() };
    candidates.dedup();
    candidates.into_iter().map(|candidate| chord_string(candidate.as_str(), mods)).collect()
}

fn chord_string(key_name: &str, mods: ModifiersState) -> String {
    let mut parts: Vec<String> = Vec::new();
    if mods.super_key() {
        parts.push("super".into());
    }
    if mods.control_key() {
        parts.push("ctrl".into());
    }
    if mods.alt_key() {
        parts.push("alt".into());
    }
    if mods.shift_key() {
        parts.push("shift".into());
    }
    parts.push(key_name.to_string());
    parts.join("+").to_ascii_lowercase()
}

fn key_candidates(key: &Key) -> Option<Vec<KeyName>> {
    let primary = key_name(key)?;
    let mut candidates = Vec::new();
    if let Key::Character(s) = key {
        if s == "?" {
            candidates.push(KeyName::Static("/"));
        } else if s.chars().count() == 1 {
            let lower = s.to_ascii_lowercase();
            if lower != *s {
                candidates.push(KeyName::Owned(lower));
            }
        }
    }
    candidates.push(primary);
    Some(candidates)
}

#[doc(hidden)]
#[doc(hidden)]
pub fn key_name(key: &Key) -> Option<KeyName> {
    Some(match key {
        Key::Named(n) => KeyName::Static(match n {
            NamedKey::Enter => "enter",
            NamedKey::Backspace => "backspace",
            NamedKey::Tab => "tab",
            NamedKey::Escape => "escape",
            NamedKey::Space => "space",
            NamedKey::ArrowUp => "up",
            NamedKey::ArrowDown => "down",
            NamedKey::ArrowRight => "right",
            NamedKey::ArrowLeft => "left",
            NamedKey::Home => "home",
            NamedKey::End => "end",
            NamedKey::PageUp => "pageup",
            NamedKey::PageDown => "pagedown",
            NamedKey::Delete => "delete",
            NamedKey::F1 => "f1",
            NamedKey::F2 => "f2",
            NamedKey::F3 => "f3",
            NamedKey::F4 => "f4",
            _ => return None,
        }),
        Key::Character(s) => KeyName::Owned(s.to_string()),
        _ => return None,
    })
}

#[doc(hidden)]
#[doc(hidden)]
#[derive(PartialEq, Eq)]
pub enum KeyName {
    Static(&'static str),
    Owned(String),
}

impl KeyName {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Static(s) => s,
            Self::Owned(s) => s.as_str(),
        }
    }
}

#[cfg(test)]
#[path = "key_encoding/tests.rs"]
mod tests;
