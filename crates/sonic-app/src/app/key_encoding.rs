//! Keyboard event → byte encoding for the PTY side, plus
//! `KeyName` mapping used by the keymap parser.
//!
//! Extracted from `app/mod.rs` in refactor PR 8b.

use sonic_core::keymap::ScrollAction;
use winit::{
    event::KeyEvent,
    keyboard::{Key, ModifiersState, NamedKey},
};

pub(super) fn _scroll_used(_a: ScrollAction) {}

pub(super) fn encode_key(event: &KeyEvent, mods: ModifiersState) -> Option<Vec<u8>> {
    encode_logical(&event.logical_key, mods)
}

#[doc(hidden)]
#[doc(hidden)]
pub fn encode_logical(key: &Key, mods: ModifiersState) -> Option<Vec<u8>> {
    let ctrl = mods.control_key();
    match key {
        Key::Named(n) => Some(match n {
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
            NamedKey::F1 => b"\x1bOP".to_vec(),
            NamedKey::F2 => b"\x1bOQ".to_vec(),
            NamedKey::F3 => b"\x1bOR".to_vec(),
            NamedKey::F4 => b"\x1bOS".to_vec(),
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

pub(super) fn key_event_to_string(event: &KeyEvent, mods: ModifiersState) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if mods.super_key() || mods.control_key() {
        parts.push("super".into());
    }
    if mods.alt_key() {
        parts.push("alt".into());
    }
    if mods.shift_key() {
        parts.push("shift".into());
    }
    let name = key_name(&event.logical_key)?;
    parts.push(name.as_str().to_string());
    Some(parts.join("+").to_ascii_lowercase())
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
