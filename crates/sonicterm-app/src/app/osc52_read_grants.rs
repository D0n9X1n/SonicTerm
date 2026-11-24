//! OSC 5522 session grants: what Always and Ban on a consent page remember for the rest of a pane's
//! session, and the credential a program offers to be remembered by.
//!
//! A program names itself with `name` and `pw`; only a nonempty `name` together with a nonempty
//! `pw` makes a [`GrantCredential`], so a lone `pw` counts as no password before any lookup or
//! grant. The name is display-only: a grant is keyed by the pane, its PTY incarnation, owner and
//! reset epochs, the operation, the location and the password. A pane keeps at most
//! [`MAX_GRANTS_PER_PANE`] grants, evicting its oldest. Neither the password nor the name is ever
//! printed by `Debug` or logged.

use sonicterm_vt::vt::KittyClipboardLocation;

use super::osc52_read_ticket::ReadBinding;

/// Grants one pane keeps at most; recording another evicts that pane's oldest.
pub(in crate::app) const MAX_GRANTS_PER_PANE: usize = 16;

/// Characters of the program-supplied name a consent page shows at most.
const MAX_DISPLAY_NAME_CHARS: usize = 64;

/// The `name` and `pw` a program offered, both nonempty.
#[derive(Clone, PartialEq, Eq)]
pub(in crate::app) struct GrantCredential {
    name: String,
    password: String,
}

impl GrantCredential {
    /// The credential `name` and `password` make; `None` when either is empty, so no grant is
    /// looked up or created for it.
    pub(in crate::app) fn new(name: &str, password: &str) -> Option<Self> {
        (!name.is_empty() && !password.is_empty())
            .then(|| Self { name: name.to_owned(), password: password.to_owned() })
    }

    /// The name as a consent page shows it: at most 64 characters, control characters replaced.
    pub(in crate::app) fn display_name(&self) -> String {
        let shown = self.name.chars().take(MAX_DISPLAY_NAME_CHARS);
        shown
            .map(|character| {
                if character.is_control() {
                    char::REPLACEMENT_CHARACTER
                } else {
                    // When: character is not a control character, it is shown as sent.
                    character
                }
            })
            .collect()
    }
}

impl std::fmt::Debug for GrantCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The name and password can identify or authorize a program, so only lengths are printed.
        formatter
            .debug_struct("GrantCredential")
            .field("name_bytes", &self.name.len())
            .field("password_bytes", &self.password.len())
            .finish()
    }
}

/// What a grant remembers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum GrantDecision {
    /// Allow matching requests without a page.
    Always,
    /// Refuse matching requests `EPERM` without a page.
    Ban,
}

/// Everything a grant is bound to. `Debug` omits the password.
#[derive(Clone, PartialEq, Eq)]
pub(in crate::app) struct GrantKey {
    pub(in crate::app) pane_id: u64,
    pub(in crate::app) incarnation: u64,
    pub(in crate::app) owner_epoch: u64,
    pub(in crate::app) reset_epoch: u64,
    write: bool,
    location: KittyClipboardLocation,
    password: String,
}

impl GrantKey {
    /// The key a request admitted under `binding` for `write` at `location` with `credential` has.
    pub(in crate::app) fn new(
        binding: &ReadBinding,
        write: bool,
        location: KittyClipboardLocation,
        credential: &GrantCredential,
    ) -> Self {
        Self {
            pane_id: binding.pane_id,
            incarnation: binding.incarnation,
            owner_epoch: binding.owner_epoch,
            reset_epoch: binding.reset_epoch,
            write,
            location,
            password: credential.password.clone(),
        }
    }
}

impl std::fmt::Debug for GrantKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The password authorizes a program, so it is never printed.
        formatter
            .debug_struct("GrantKey")
            .field("pane_id", &self.pane_id)
            .field("write", &self.write)
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

/// Every pane's grants, oldest first.
#[derive(Debug, Default)]
pub(in crate::app) struct SessionGrants {
    entries: Vec<(GrantKey, GrantDecision)>,
}

impl SessionGrants {
    /// The decision recorded for exactly `key`.
    pub(in crate::app) fn lookup(&self, key: &GrantKey) -> Option<GrantDecision> {
        self.entries.iter().find(|(entry, _)| entry == key).map(|(_, decision)| *decision)
    }

    /// Record `decision` for `key`, replacing an earlier one; past [`MAX_GRANTS_PER_PANE`] for the
    /// pane, its oldest grant is evicted first.
    pub(in crate::app) fn record(&mut self, key: GrantKey, decision: GrantDecision) {
        self.entries.retain(|(entry, _)| *entry != key);
        let pane_id = key.pane_id;
        let held = self.entries.iter().filter(|(entry, _)| entry.pane_id == pane_id).count();
        if held >= MAX_GRANTS_PER_PANE {
            // A pane at MAX_GRANTS_PER_PANE grants evicts its oldest first.
            if let Some(oldest) =
                self.entries.iter().position(|(entry, _)| entry.pane_id == pane_id)
            {
                self.entries.remove(oldest);
            }
        }
        self.entries.push((key, decision));
    }

    /// Keep only the grants `live` accepts.
    pub(in crate::app) fn retain(&mut self, mut live: impl FnMut(&GrantKey) -> bool) {
        self.entries.retain(|(key, _)| live(key));
    }

    /// Drop every grant, as a policy change does.
    pub(in crate::app) fn clear(&mut self) {
        self.entries.clear();
    }

    /// How many grants are held.
    pub(in crate::app) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no grant is held.
    pub(in crate::app) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
#[path = "osc52_read_grants_tests.rs"]
mod osc52_read_grants_tests;
