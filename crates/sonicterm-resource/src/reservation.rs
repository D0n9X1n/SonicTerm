use crate::ledger::Ledger;
use sonicterm_types::{BudgetError, ResourceAmount, ResourceClass, ResourceOwnerId};
use std::{fmt, sync::Arc};

pub(crate) struct Charge {
    pub(crate) ledger: Arc<Ledger>,
    pub(crate) owner: ResourceOwnerId,
    pub(crate) class: ResourceClass,
    pub(crate) amount: ResourceAmount,
}

impl Charge {
    fn release(&self) {
        // A release that cannot be applied is counted rather than asserted.
        // Panicking here would fire inside `Drop`, which turns a recoverable
        // accounting fault into an abort during unwind, and would leave the
        // path untestable in exactly the builds that ship.
        if self.ledger.release(self.owner, self.class, self.amount).is_err() {
            self.ledger.record_release_failure();
        }
    }
}

/// RAII token for a pre-allocation resource reservation.
pub struct Reservation {
    pub(crate) charge: Option<Charge>,
}

impl Reservation {
    pub(crate) fn new(charge: Charge) -> Self {
        Self { charge: Some(charge) }
    }

    fn charge(&self) -> &Charge {
        self.charge.as_ref().expect("live reservation charge")
    }

    /// Amount currently reserved by this token.
    pub fn reserved_amount(&self) -> ResourceAmount {
        self.charge().amount
    }

    /// Commit an actual amount no greater than the reservation.
    pub fn commit(mut self, actual: ResourceAmount) -> Result<CommittedReservation, CommitError> {
        let current = self.charge().amount;
        if !actual.component_le(current) {
            // When: actual exceeds current on an accounting axis; commit cannot
            // retain capacity that admission never checked.
            return Err(CommitError {
                reservation: self,
                error: BudgetError::AmountExceedsCharge { requested: actual, available: current },
            });
        }
        let difference = current.checked_sub(actual).expect("component-wise checked");
        let charge = self.charge();
        // Committing settles a charge that was already admitted, so it does not
        // re-check admission state: closing an owner rejects new reservations while
        // still letting live tokens finalize during teardown.
        let result = charge.ledger.release(charge.owner, charge.class, difference);
        if let Err(error) = result {
            // When: result contains error before mutation, so the original full
            // token remains the sole RAII owner and returns unchanged.
            return Err(CommitError { reservation: self, error });
        }
        let mut charge = self.charge.take().expect("live reservation charge");
        charge.amount = actual;
        Ok(CommittedReservation { charge: Some(charge) })
    }

    /// Split an independent child reservation from this token.
    pub fn split(&mut self, amount: ResourceAmount) -> Result<Reservation, BudgetError> {
        let current = self.charge().amount;
        let remainder = current.checked_sub(amount)?;
        self.charge.as_mut().expect("live reservation charge").amount = remainder;
        let charge = self.charge();
        Ok(Reservation::new(Charge {
            ledger: charge.ledger.clone(),
            owner: charge.owner,
            class: charge.class,
            amount,
        }))
    }

    /// Atomically transfer attribution to another owner and class.
    pub fn transfer(
        mut self,
        owner: ResourceOwnerId,
        class: ResourceClass,
    ) -> Result<Reservation, TransferError> {
        let charge = self.charge();
        if let Err(error) =
            charge.ledger.transfer(charge.owner, charge.class, owner, class, charge.amount)
        {
            // When: transfer returns error with ledger attribution unchanged;
            // charge remains source-attributed for retry or release.
            return Err(TransferError { reservation: self, error });
        }
        let charge = self.charge.as_mut().expect("live reservation charge");
        charge.owner = owner;
        charge.class = class;
        Ok(self)
    }
}

// Lifecycle: Reservation owns charge until Drop uses take and release; successful
// ownership moves consume it first, preserving exact-once release.
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(charge) = self.charge.take() {
            charge.release();
        }
    }
}

impl fmt::Debug for Reservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let charge = self.charge();
        formatter
            .debug_struct("Reservation")
            .field("owner", &charge.owner)
            .field("class", &charge.class)
            .field("amount", &charge.amount)
            .finish()
    }
}

/// RAII token for an allocation's retained committed charge.
pub struct CommittedReservation {
    pub(crate) charge: Option<Charge>,
}

impl CommittedReservation {
    fn charge(&self) -> &Charge {
        self.charge.as_ref().expect("live committed charge")
    }

    /// Amount currently committed by this token.
    pub fn committed_amount(&self) -> ResourceAmount {
        self.charge().amount
    }

    /// Shrink this charge to a component-wise smaller total.
    pub fn shrink(&mut self, actual: ResourceAmount) -> Result<(), BudgetError> {
        let current = self.charge().amount;
        if !actual.component_le(current) {
            // When: actual is not component_le current; shrink cannot increase an
            // axis through a release path that skips admission.
            return Err(BudgetError::InvalidResize {
                operation: sonicterm_types::ResizeOperation::Shrink,
                current,
                requested: actual,
            });
        }
        let difference = current.checked_sub(actual)?;
        let charge = self.charge();
        charge.ledger.release(charge.owner, charge.class, difference)?;
        self.charge.as_mut().expect("live committed charge").amount = actual;
        Ok(())
    }

    /// Grow this charge to a component-wise larger total.
    pub fn try_grow(&mut self, actual: ResourceAmount) -> Result<(), BudgetError> {
        let current = self.charge().amount;
        if !current.component_le(actual) {
            // When: current is not component_le actual; a mixed or downward
            // request needs release semantics this grow path cannot combine safely.
            return Err(BudgetError::InvalidResize {
                operation: sonicterm_types::ResizeOperation::Grow,
                current,
                requested: actual,
            });
        }
        let difference = actual.checked_sub(current)?;
        let charge = self.charge();
        charge.ledger.reserve(charge.owner, charge.class, difference)?;
        self.charge.as_mut().expect("live committed charge").amount = actual;
        Ok(())
    }

    /// Split an independent committed charge from this token.
    pub fn split(&mut self, amount: ResourceAmount) -> Result<CommittedReservation, BudgetError> {
        let current = self.charge().amount;
        let remainder = current.checked_sub(amount)?;
        self.charge.as_mut().expect("live committed charge").amount = remainder;
        let charge = self.charge();
        Ok(CommittedReservation {
            charge: Some(Charge {
                ledger: charge.ledger.clone(),
                owner: charge.owner,
                class: charge.class,
                amount,
            }),
        })
    }

    /// Atomically transfer same-ledger, same-owner charges while preserving classes.
    pub fn transfer_batch<'a>(
        reservations: impl IntoIterator<Item = &'a mut CommittedReservation>,
        owner: ResourceOwnerId,
    ) -> Result<(), CommittedBatchTransferError> {
        let reservations: Vec<_> = reservations.into_iter().collect();
        let Some(first) = reservations.first() else {
            // When: `reservations.first()` is `None`, there is no attribution to
            // validate or move.
            return Ok(());
        };
        let ledger = first.charge().ledger.clone();
        let source = first.charge().owner;
        let mut amounts: enum_map::EnumMap<ResourceClass, ResourceAmount> =
            enum_map::EnumMap::default();
        for reservation in &reservations {
            let charge = reservation.charge();
            if !Arc::ptr_eq(&ledger, &charge.ledger) {
                // When: a token belongs to another governor, no single ledger can
                // commit the batch atomically.
                return Err(CommittedBatchTransferError::MixedLedger);
            }
            if charge.owner != source {
                // When: source owners differ, moving one aggregate would subtract
                // the wrong owner path for at least one token.
                return Err(CommittedBatchTransferError::MixedSource {
                    expected: source,
                    actual: charge.owner,
                });
            }
            amounts[charge.class] = amounts[charge.class]
                .checked_add(charge.amount)
                .map_err(CommittedBatchTransferError::Budget)?;
        }
        ledger
            .transfer_batch(source, owner, &amounts)
            .map_err(CommittedBatchTransferError::Budget)?;
        for reservation in reservations {
            reservation.charge.as_mut().expect("live committed charge").owner = owner;
        }
        Ok(())
    }

    /// Atomically transfer this committed charge to another owner and class.
    pub fn transfer(
        mut self,
        owner: ResourceOwnerId,
        class: ResourceClass,
    ) -> Result<CommittedReservation, CommittedTransferError> {
        let charge = self.charge();
        if let Err(error) =
            charge.ledger.transfer(charge.owner, charge.class, owner, class, charge.amount)
        {
            // When: transfer returns error and charge remains at its source; the
            // original token survives for retry or eventual release.
            return Err(CommittedTransferError { reservation: self, error });
        }
        let charge = self.charge.as_mut().expect("live committed charge");
        charge.owner = owner;
        charge.class = class;
        Ok(self)
    }
}

// Lifecycle: CommittedReservation stays charged until Drop uses charge take and
// release; transfer only rewrites attribution on the same token.
impl Drop for CommittedReservation {
    fn drop(&mut self) {
        if let Some(charge) = self.charge.take() {
            charge.release();
        }
    }
}

impl fmt::Debug for CommittedReservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let charge = self.charge();
        formatter
            .debug_struct("CommittedReservation")
            .field("owner", &charge.owner)
            .field("class", &charge.class)
            .field("amount", &charge.amount)
            .finish()
    }
}

/// Failed commit that returns the original reservation token.
#[derive(Debug)]
pub struct CommitError {
    /// Unchanged original reservation.
    pub reservation: Reservation,
    /// Rejection reason.
    pub error: BudgetError,
}

/// Failed reservation transfer that returns the original token.
#[derive(Debug)]
pub struct TransferError {
    /// Unchanged original reservation.
    pub reservation: Reservation,
    /// Rejection reason.
    pub error: BudgetError,
}

/// Failed committed transfer that returns the original committed token.
#[derive(Debug)]
pub struct CommittedTransferError {
    /// Unchanged original committed reservation.
    pub reservation: CommittedReservation,
    /// Rejection reason.
    pub error: BudgetError,
}

/// Rejection reason for an atomic committed-reservation batch transfer.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CommittedBatchTransferError {
    /// At least one token belongs to a different governor ledger.
    MixedLedger,
    /// At least one token belongs to a different source owner.
    MixedSource {
        /// Source owner established by the first token.
        expected: ResourceOwnerId,
        /// Different source owner found later in the batch.
        actual: ResourceOwnerId,
    },
    /// The shared ledger rejected the complete transfer before mutation.
    Budget(BudgetError),
}

impl fmt::Display for CommittedBatchTransferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for CommittedBatchTransferError {}

#[cfg(test)]
#[path = "reservation_tests.rs"]
mod reservation_tests;
