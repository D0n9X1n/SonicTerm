//! Leases covering the copies the native clipboard keeps of SonicTerm's writes: arboard on Linux
//! holds its own copy of each selection it wrote, PRIMARY and CLIPBOARD separately, until that
//! selection is synchronously replaced through arboard. Neither clear nor X11 takeover proves arboard
//! has processed SelectionClear and released the bytes.

use super::super::clipboard_worker::TargetOutcome;
use super::ClipLease;

/// One selection's share of a write's lease: the native copy that selection may now hold.
#[derive(Debug)]
struct RetainedShare {
    lease: ClipLease,
    /// Whether the selection surely took the write; false when its native call failed after it
    /// may have stored the text.
    sure: bool,
}

/// The shares one committed write leaves: one per selection that took it, or may have.
#[derive(Debug, Default)]
pub(in crate::app) struct RetainedShares {
    clipboard: Option<RetainedShare>,
    primary: Option<RetainedShare>,
}

impl RetainedShares {
    /// Split one native copy of `copy_bytes` off `lease` for each selection whose outcome is
    /// taken or unknown; a selection that refused the write, or was not written, gets none.
    pub(in crate::app) fn split(
        lease: &mut ClipLease,
        copy_bytes: usize,
        clipboard: TargetOutcome,
        primary: TargetOutcome,
    ) -> Self {
        let mut share = |outcome: TargetOutcome| match outcome {
            TargetOutcome::Taken | TargetOutcome::Unknown => {
                let sure = outcome == TargetOutcome::Taken;
                Some(RetainedShare { lease: lease.split_off(copy_bytes), sure })
            }
            TargetOutcome::Refused | TargetOutcome::Untouched => None,
        };
        Self { clipboard: share(clipboard), primary: share(primary) }
    }
}

/// Leases covering the native clipboard's retained copy of SonicTerm's last write to each
/// selection.
#[derive(Debug, Default)]
pub(in crate::app) struct RetainedCopies {
    clipboard: Option<ClipLease>,
    primary: Option<ClipLease>,
}

impl RetainedCopies {
    /// Settle each selection's share of a committed write against what that selection held.
    pub(in crate::app) fn retain(&mut self, shares: RetainedShares) {
        settle(&mut self.clipboard, shares.clipboard);
        settle(&mut self.primary, shares.primary);
    }

    /// A user copy replaced the clipboard selection, so its retained copy is gone; PRIMARY keeps
    /// its own.
    pub(in crate::app) fn release_clipboard(&mut self) {
        self.clipboard = None;
    }

    /// Bytes the retained leases hold.
    #[cfg(test)]
    pub(in crate::app) fn bytes(&self) -> usize {
        self.clipboard.iter().chain(&self.primary).map(ClipLease::bytes).sum()
    }
}

/// Settle one selection: a selection that surely took the write replaced its previous copy, whose
/// lease is released; one whose native call failed ambiguously may hold either copy, so it keeps
/// the larger lease; a selection with no share keeps what it held.
fn settle(held: &mut Option<ClipLease>, share: Option<RetainedShare>) {
    let Some(share) = share else {
        // When: share is None, the selection refused the write or was not written.
        return;
    };
    let larger = held.as_ref().is_none_or(|old| share.lease.bytes() >= old.bytes());
    if share.sure || larger {
        // The write surely replaced the copy, or its lease covers the larger of the two.
        *held = Some(share.lease);
    }
}
