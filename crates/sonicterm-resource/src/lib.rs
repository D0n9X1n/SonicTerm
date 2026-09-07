//! Concrete resource governor and RAII reservation tokens.

mod cancel;
mod clock;
mod ledger;
mod owner;
mod reaper;
mod reservation;

use ledger::Ledger;
use reservation::Charge;
use sonicterm_types::{
    BudgetError, GovernorLimits, OwnerKind, OwnerLimits, ProcessKind, ResourceAmount,
    ResourceClass, ResourceOwnerId, ResourceSnapshot,
};
use std::sync::Arc;

pub use cancel::{CancelSource, CancelToken};
pub use clock::{Clock, SystemClock, TestClock};
pub use reaper::{
    deadline_from, ReapAction, ReapSlot, ReapTask, ReaperLimits, ReaperProgress, ReaperSupervisor,
    ShutdownReport,
};
pub use reservation::{
    CommitError, CommittedBatchTransferError, CommittedReservation, CommittedTransferError,
    Reservation, TransferError,
};

/// Shared process-local resource governor with one immutable process root.
#[derive(Clone)]
pub struct ResourceGovernor {
    ledger: Arc<Ledger>,
}

impl ResourceGovernor {
    /// Create a governor and its process root owner.
    pub fn new(kind: ProcessKind, limits: GovernorLimits) -> Result<Self, BudgetError> {
        Ok(Self { ledger: Ledger::new(kind, limits)? })
    }

    /// Return the process root owner.
    pub fn root_owner(&self) -> ResourceOwnerId {
        self.ledger.root
    }

    /// Create a child owner below an open parent.
    pub fn create_child(
        &self,
        parent: ResourceOwnerId,
        kind: OwnerKind,
        limits: OwnerLimits,
    ) -> Result<ResourceOwnerId, BudgetError> {
        self.ledger.create_child(parent, kind, limits)
    }

    /// Stop admitting new reservations and children for an owner.
    pub fn begin_close(&self, owner: ResourceOwnerId) -> Result<(), BudgetError> {
        self.ledger.begin_close(owner)
    }

    /// Finish closing an owner with no charges or open children.
    pub fn finish_close(&self, owner: ResourceOwnerId) -> Result<(), BudgetError> {
        self.ledger.finish_close(owner)
    }

    /// Reserve bytes and items before retaining or allocating a resource.
    pub fn try_reserve(
        &self,
        owner: ResourceOwnerId,
        class: ResourceClass,
        amount: ResourceAmount,
    ) -> Result<Reservation, BudgetError> {
        self.ledger.reserve(owner, class, amount)?;
        Ok(Reservation::new(Charge { ledger: self.ledger.clone(), owner, class, amount }))
    }

    /// Hand the shared ledger to `test_support` so it can construct a charge
    /// the ledger never issued.
    ///
    /// Feature-gated: this exists only to reach the permanently-inconsistent
    /// accounting state, which a correct ledger cannot produce and which
    /// downstream crates otherwise cannot test their reporting against.
    #[cfg(feature = "test-util")]
    pub(crate) fn ledger_for_test_support(&self) -> Arc<Ledger> {
        self.ledger.clone()
    }

    /// Return an observational owner/process snapshot.
    pub fn snapshot(&self, owner: ResourceOwnerId) -> Result<ResourceSnapshot, BudgetError> {
        self.ledger.snapshot(owner)
    }

    #[cfg(test)]
    fn with_next_owner_id(
        kind: ProcessKind,
        limits: GovernorLimits,
        next_owner_id: u64,
    ) -> Result<Self, BudgetError> {
        Ok(Self { ledger: Ledger::new_with_next_id(kind, limits, next_owner_id)? })
    }
}

#[cfg(feature = "test-util")]
pub mod test_support;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
