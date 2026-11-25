//! One X11 offer. Atoms index the staged buffers; an INCR cursor borrows a slice on each delete,
//! so neither text aliases nor simultaneous requestors need private copies of the payload.

use super::{CommitGate, NativeFormat, NativeOffer};
use crate::app::clipboard_worker::{TargetOutcome, WriteResult};
use crate::app::osc52_stream::{ClipLease, ClipTargets};
use std::time::{Duration, Instant};

const TEXT_NAMES: [&str; 5] =
    ["UTF8_STRING", "text/plain;charset=utf-8", "text/plain", "STRING", "TEXT"];
const MAX_REQUESTORS: usize = 8;
const CHUNK_WAIT: Duration = Duration::from_secs(5);
const WRITE_LIFETIME: Duration = Duration::from_secs(3600);
const CHUNK_BYTES: usize = 256 * 1024;

/// Process-lifetime names, stored inline so an untrusted name cannot grow an uncharged cache.
struct AtomNames {
    admitted_names: std::sync::Mutex<([[u8; 255]; 256], usize)>,
}

impl AtomNames {
    #[cfg(test)]
    const fn new() -> Self {
        Self::with_fixed(0)
    }

    const fn with_fixed(fixed: usize) -> Self {
        Self { admitted_names: std::sync::Mutex::new(([[0; 255]; 256], fixed)) }
    }

    fn admit(&self, name: &str) -> bool {
        let (slots, used) =
            &mut *self.admitted_names.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if slots[..*used].iter().any(|slot| {
            slot.starts_with(name.as_bytes()) && slot.get(name.len()).copied().unwrap_or(0) == 0
        }) {
            // When: slots already holds name, the atom can be reused even at the cap.
            return true;
        }
        let Some(slot) = slots.get_mut(*used) else {
            // When: slots is full, preflight omits the new name without interning it.
            return false;
        };
        slot[..name.len()].copy_from_slice(name.as_bytes());
        *used += 1;
        true
    }
}

/// Eight data atoms are reserved for the built-in formats, separate from protocol/control atoms.
const FIXED_DATA_ATOMS: usize = TEXT_NAMES.len() + 3;

fn holds_with(display: Option<&std::ffi::OsStr>, names: &AtomNames, format: &NativeFormat) -> bool {
    display.is_some_and(|display| !display.is_empty())
        && match format {
            NativeFormat::Custom(name) => names.admit(name),
            _ => true,
        }
}

/// Whether the process can offer this format through its configured X display.
#[cfg(target_os = "linux")]
pub(super) fn holds(format: &NativeFormat) -> bool {
    static NAMES: AtomNames = AtomNames::with_fixed(FIXED_DATA_ATOMS);
    holds_with(std::env::var_os("DISPLAY").as_deref(), &NAMES, format)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Destination {
    window: u32,
    property: u32,
}

/// The properties the selection protocol writes, distinct from the event loop delivering requests.
trait OwnerPort {
    fn intern(&mut self, name: &str) -> Option<u32>;
    fn bytes(&mut self, dest: Destination, kind: u32, bytes: &[u8]) -> bool;
    fn words(&mut self, dest: Destination, kind: u32, words: &[u32]) -> bool;
    fn watch(&mut self, window: u32, enable: bool) -> bool;
}

/// Selection acquisition and its independent server confirmation.
trait OwnershipPort {
    fn set_owner(&mut self, window: u32, selection: u32, stamp: u32) -> bool;
    fn owner(&mut self, selection: u32) -> Option<u32>;
}

/// A failed confirmation must never clear another client's selection, including when a later
/// client won the race between SetSelectionOwner and GetSelectionOwner.
fn claim(
    port: &mut impl OwnershipPort,
    window: u32,
    selections: [u32; 2],
    stamp: u32,
    gate: CommitGate<'_>,
) -> WriteResult {
    let targets = ClipTargets { clipboard: selections[0] != 0, primary: selections[1] != 0 };
    let mut result = WriteResult::each(targets, TargetOutcome::Refused);
    if !gate() {
        // When: gate refuses before the first SetSelectionOwner, both selections are untouched.
        return WriteResult::stale();
    }
    for (selection, outcome) in
        selections.into_iter().zip([&mut result.clipboard, &mut result.primary])
    {
        if selection == 0 {
            // When: selection is zero, this destination was not requested.
            continue;
        }
        if !port.set_owner(window, selection, stamp) || port.owner(selection) != Some(window) {
            // When: set_owner failed or owner names another window, keep only already confirmed selections.
            break;
        }
        *outcome = TargetOutcome::Taken;
    }
    result
}

/// Install the window on each confirmed selection, even when its sibling was refused. An older
/// window on this connection need not receive SelectionClear, so retire those selections locally.
fn install_claim(
    offers: &mut std::collections::HashMap<u32, Active>,
    port: &mut impl OwnershipPort,
    window: u32,
    mut active: Active,
    gate: CommitGate<'_>,
) -> WriteResult {
    let result = claim(port, window, active.selections, active.owner.stamp, gate);
    for (atom, outcome) in active.selections.iter_mut().zip([result.clipboard, result.primary]) {
        if outcome == TargetOutcome::Taken {
            for old in offers.values_mut() {
                old.retire(*atom);
            }
        } else {
            // When: outcome was not Taken, this window must not serve the selection.
            *atom = 0;
        }
    }
    if active.selections != [0; 2] {
        offers.insert(window, active);
    }
    result
}

struct Active {
    owner: Owner,
    selections: [u32; 2],
}

impl Active {
    fn accepts(&self, selection: u32) -> bool {
        selection != 0 && self.selections.contains(&selection)
    }

    fn retire(&mut self, lost: u32) {
        for selection in &mut self.selections {
            if *selection == lost {
                *selection = 0;
            }
        }
        if self.selections == [0; 2] {
            self.owner.clear();
        }
    }
}

struct Cursor {
    deadline: Instant,
    dest: Destination,
    target: u32,
    index: usize,
    offset: usize,
}

struct Owner {
    offer: NativeOffer,
    /// STRING alone may need a different encoding; its index is one past the original buffers.
    latin1: Option<Vec<u8>>,
    /// TARGETS and TIMESTAMP precede the data atoms; indices maps each data atom to its payload.
    atoms: Vec<u32>,
    indices: Vec<usize>,
    atom_type: u32,
    integer_type: u32,
    incr: u32,
    stamp: u32,
    direct_bytes: usize,
    expires: Instant,
    accepting: bool,
    cursors: Vec<Cursor>,
    _lease: ClipLease,
}

fn data_atoms(offer: &NativeOffer) -> usize {
    offer.representations.len()
        + usize::from(offer.representations.iter().any(|(format, _)| *format == NativeFormat::Text))
            * 4
}

/// Bytes retained beside the payload: both plan vectors, atom and index tables, eight cursors
/// and one format-32 wire buffer. INCR writes borrow the already reserved bytes directly.
pub(super) fn owner_workspace(offer: &NativeOffer) -> usize {
    let count = data_atoms(offer);
    raw_tables(count)
        + offer.entries.capacity() * size_of::<Vec<u8>>()
        + offer.representations.capacity() * size_of::<(NativeFormat, usize)>()
}

/// Index table, atom list and its format-32 wire copy, and the bounded set of INCR cursors.
pub(super) fn raw_tables(count: usize) -> usize {
    count * size_of::<usize>()
        + (count + 2) * 2 * size_of::<u32>()
        + MAX_REQUESTORS * size_of::<Cursor>()
}

/// Encode ICCCM STRING only for text representable as Latin-1 graphics, TAB or LF. ASCII can
/// reuse the original buffer. The optional encoded buffer never exceeds the reserved text copy.
fn string_index(
    raw: &[u8],
    index: usize,
    extra: usize,
    latin1: &mut Option<Vec<u8>>,
) -> Option<usize> {
    let text = std::str::from_utf8(raw).ok()?;
    if !text.chars().all(|scalar| matches!(scalar, '\t' | '\n' | ' '..='~' | '\u{a0}'..='\u{ff}')) {
        // When: text includes a scalar outside the ICCCM repertoire, do not advertise STRING.
        return None;
    }
    if text.is_ascii() {
        // When: text is ASCII, STRING can share its original bytes.
        return Some(index);
    }
    let mut bytes = Vec::with_capacity(text.chars().count());
    bytes.extend(text.chars().map(|scalar| scalar as u8));
    *latin1 = Some(bytes);
    Some(extra)
}

impl Owner {
    fn data(&self, index: usize) -> &[u8] {
        if index == self.offer.entries.len() {
            self.latin1.as_deref().expect("encoded STRING index")
        } else {
            // When: index names a staged entry, no Latin-1 conversion is needed.
            &self.offer.entries[index]
        }
    }
    /// Lifecycle: lease precedes offer so any early return drops the payload before its reservation.
    fn stage(
        port: &mut impl OwnerPort,
        mut lease: ClipLease,
        offer: NativeOffer,
        stamp: u32,
        direct_bytes: usize,
        now: Instant,
    ) -> Option<Self> {
        let count = data_atoms(&offer);
        if !lease.grow_to(offer.reserved_bytes().max(owner_workspace(&offer))) {
            // When: lease cannot cover the offer table, do not allocate or take ownership.
            return None;
        }
        let mut atoms = Vec::with_capacity(count + 2);
        let mut indices = Vec::with_capacity(count);
        atoms.extend([port.intern("TARGETS")?, port.intern("TIMESTAMP")?]);
        let mut latin1 = None;
        for (format, index) in &offer.representations {
            let single: [&str; 1];
            let names: &[&str] = match format {
                NativeFormat::Text => &TEXT_NAMES,
                NativeFormat::Html => &["text/html"],
                NativeFormat::Png => &["image/png"],
                NativeFormat::UriList => &["text/uri-list"],
                NativeFormat::Custom(name) => {
                    single = [name];
                    &single
                }
            };
            for name in names {
                let index = if *name == "STRING" {
                    // When: name is STRING, the wire encoding is Latin-1 rather than raw UTF-8.
                    let Some(source) = string_index(
                        &offer.entries[*index],
                        *index,
                        offer.entries.len(),
                        &mut latin1,
                    ) else {
                        // When: string_index has no encoding, omit just that target.
                        continue;
                    };
                    source
                } else {
                    // When: name is not STRING, preserve the original representation.
                    *index
                };
                atoms.push(port.intern(name)?);
                indices.push(index);
            }
        }
        // Finish all fallible staging before moving either parameter into the result.
        let atom_type = port.intern("ATOM")?;
        let integer_type = port.intern("INTEGER")?;
        let incr = port.intern("INCR")?;
        Some(Self {
            offer,
            _lease: lease,
            latin1,
            atoms,
            indices,
            atom_type,
            integer_type,
            incr,
            stamp,
            direct_bytes,
            expires: now + WRITE_LIFETIME,
            accepting: true,
            cursors: Vec::with_capacity(MAX_REQUESTORS),
        })
    }

    fn clear(&mut self) {
        self.accepting = false;
    }

    fn live(&self, now: Instant) -> bool {
        if self.accepting {
            now < self.expires
        } else {
            // When: accepting is false, the last admitted cursor alone keeps the offer alive.
            !self.cursors.is_empty()
        }
    }

    fn expire(&mut self, port: &mut impl OwnerPort, now: Instant) {
        self.cursors.retain(|cursor| {
            if now >= cursor.deadline {
                // The deadline has passed, a late delete cannot revive the transfer.
                port.watch(cursor.dest.window, false);
            }
            now < cursor.deadline
        });
    }

    fn request(
        &mut self,
        port: &mut impl OwnerPort,
        dest: Destination,
        target: u32,
        now: Instant,
    ) -> bool {
        self.expire(port, now);
        if !self.accepting
            || now >= self.expires
            || self.cursors.len() >= MAX_REQUESTORS
            || self.cursors.iter().any(|cursor| cursor.dest.window == dest.window)
        {
            // When: not accepting, expired, full or already serving this window, do not admit a new request.
            return false;
        }
        let Some(slot) = self.atoms.iter().position(|atom| *atom == target) else {
            // When: target is absent from atoms, the conversion is refused.
            return false;
        };
        match slot {
            0 => port.words(dest, self.atom_type, &self.atoms),
            1 => port.words(dest, self.integer_type, &[self.stamp]),
            _ => {
                // When: slot names data rather than metadata, its index selects the staged bytes.
                let index = self.indices[slot - 2];
                let bytes = self.data(index);
                if bytes.len() <= self.direct_bytes {
                    // When: bytes fit direct_bytes, the reply fits one ChangeProperty request.
                    return port.bytes(dest, target, bytes);
                }
                if !port.watch(dest.window, true) {
                    // When: watch fails, property deletes cannot drive an INCR transfer.
                    return false;
                }
                if !port.words(dest, self.incr, &[bytes.len() as u32]) {
                    // When: the INCR words cannot be stored, the requestor has no transfer.
                    port.watch(dest.window, false);
                    return false;
                }
                self.cursors.push(Cursor {
                    dest,
                    target,
                    index,
                    offset: 0,
                    deadline: now + CHUNK_WAIT,
                });
                true
            }
        }
    }

    fn deleted(&mut self, port: &mut impl OwnerPort, dest: Destination, now: Instant) {
        self.expire(port, now);
        let Some(slot) = self.cursors.iter().position(|cursor| cursor.dest == dest) else {
            // When: dest has no cursor, the deletion belongs to another property.
            return;
        };
        let cursor = &self.cursors[slot];
        let bytes = self.data(cursor.index);
        let end = bytes.len().min(cursor.offset + CHUNK_BYTES.min(self.direct_bytes));
        let chunk = &bytes[cursor.offset..end];
        if !port.bytes(dest, cursor.target, chunk) || chunk.is_empty() {
            // If the write fails or chunk is the terminator, no further delete is needed.
            self.cursors.swap_remove(slot);
            port.watch(dest.window, false);
        } else {
            // When: bytes wrote a nonempty chunk, extend only this cursor's deadline.
            let cursor = &mut self.cursors[slot];
            cursor.offset = end;
            cursor.deadline = now + CHUNK_WAIT;
        }
    }
}

#[cfg(test)]
#[path = "kitty_clipboard_offer_x11_tests.rs"]
mod tests;

#[cfg(target_os = "linux")]
#[path = "kitty_clipboard_offer_x11_native.rs"]
mod native;

#[cfg(target_os = "linux")]
pub(in crate::app) use native::SelectionServer;
