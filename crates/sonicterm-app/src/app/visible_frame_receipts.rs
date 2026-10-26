//! Generation-qualified receipt binding and held-grid acknowledgement.

use super::*;

impl VisibleFrameSources {
    /// Bind a presented frame's receipts to the panes this collection held, by index; a receipt whose
    /// index or pane id disagrees with the layout is dropped.
    pub(in crate::app) fn bind(&self, receipts: Vec<AckReceipt>) -> Vec<AckTicket> {
        receipts
            .into_iter()
            .filter_map(|receipt| {
                let entry =
                    self.entries.get(receipt.index).filter(|entry| entry.id == receipt.pane_id)?;
                Some(AckTicket { receipt, parser: Arc::downgrade(&entry.parser) })
            })
            .collect()
    }

    /// Store a frame's receipts as the window's pending set. Receipts are non-empty only for a
    /// presented frame, which replaces the set; any other outcome leaves the set unchanged.
    pub(in crate::app) fn store_presented(
        &self,
        pending: &mut Vec<AckTicket>,
        receipts: Vec<AckReceipt>,
    ) {
        if receipts.is_empty() {
            // When: `receipts` is empty the frame did not present, so the pending set stays as it was.
            return;
        }
        *pending = self.bind(receipts);
    }

    /// Reconcile the viewports, then, only when that succeeds, apply `window`'s pending receipts to
    /// the held grids and empty the set. A failed reconciliation returns before any receipt is
    /// touched, so the set and every grid's dirt stay for the next successful collection.
    pub(in crate::app) fn reconcile_and_apply_receipts(
        &self,
        window: &mut WindowState,
        guards: &mut ParserGuards<'_>,
    ) -> Result<FrameViewports, FrameUnavailable> {
        let viewports = self.reconcile_viewports(&mut window.panes, guards)?;
        let pending = std::mem::take(&mut window.pending_receipts);
        let dropped = pending.iter().filter(|ticket| !apply_ticket(ticket, guards)).count();
        if dropped > 0 {
            if let Some(counters) = window.redraw.frame_counters.as_deref_mut() {
                counters.dirt_ack_dropped += dropped as u64;
            }
        }
        Ok(viewports)
    }
}

/// Apply one pending ticket through the guard the collection already holds; returns whether it
/// cleared. It is live only when a guard holds its pane and that guard's parser is the ticket's.
pub(in crate::app) fn apply_ticket(ticket: &AckTicket, guards: &mut ParserGuards<'_>) -> bool {
    let Some((_, parser, _)) = guards.iter_mut().find(|(id, _, _)| *id == ticket.receipt.pane_id)
    else {
        // When: no held guard has the receipt's `pane_id` (switched away, removed or moved), its dirt stays.
        return false;
    };
    if !std::ptr::eq(MutexGuard::mutex(parser), ticket.parser.as_ptr()) {
        // When: the id now names another parser, the receipt describes a pane that no longer exists.
        return false;
    }
    ticket.receipt.try_apply(parser.grid_mut())
}

#[cfg(test)]
#[path = "visible_frame_receipts_tests.rs"]
mod visible_frame_receipts_tests;
