//! Delivery receipts, retry scheduling and typed admission outcomes shared across resource owners.

use super::BudgetError;
use std::time::Instant;

/// Opaque identifier for accepted asynchronous delivery work.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DeliveryReceipt(u64);

impl DeliveryReceipt {
    /// Construct a delivery receipt identifier.
    #[inline]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the opaque numeric identifier.
    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Signal that should make a backpressured caller retry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum RetryWakeup {
    /// Retry when the token deadline is reached.
    AtDeadline,
    /// Retry when the receiver reports newly available capacity.
    CapacityAvailable,
    /// Retry after connection or attachment state changes.
    ConnectionStateChanged,
}

/// Provider-neutral retry scheduling information.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RetryToken {
    deadline: Instant,
    wakeup: RetryWakeup,
}

impl RetryToken {
    /// Construct retry scheduling information.
    #[inline]
    pub const fn new(deadline: Instant, wakeup: RetryWakeup) -> Self {
        Self { deadline, wakeup }
    }

    /// Earliest retry deadline.
    #[inline]
    pub const fn deadline(self) -> Instant {
        self.deadline
    }

    /// Event that should wake the retry owner.
    #[inline]
    pub const fn wakeup(self) -> RetryWakeup {
        self.wakeup
    }
}

/// Reasons a class-specific pressure policy may drop semantically lossy data.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum DropReason {
    /// Frozen pressure policy explicitly permits dropping this class.
    Policy,
    /// Superseded coalescible state was replaced by newer state.
    Superseded,
}

/// Why a specific admission was refused or an allocation reclaimed.
///
/// [`BudgetError`] says why a *governor operation* failed and [`DropReason`]
/// says why a pressure policy discarded data. Neither answers the question a
/// diagnostic leaves open: this pane asked to keep something and did not get
/// to — why?
///
/// Distinct variants exist wherever the *remedy* differs. `PerOwnerBudget` and
/// `ProcessBudget` both mean "no room", but the first is relieved by that
/// owner releasing something and the second only by the process as a whole
/// coming down; an operator who cannot tell them apart cannot act.
/// `ItemTooLarge` is neither — no amount of reclamation admits it, so retrying
/// after a sweep is wasted work.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum AdmissionRejection {
    /// The item exceeds a per-item limit and can never be admitted.
    ///
    /// Reclamation cannot help: the item is larger than the policy allows
    /// regardless of what else is retained. Callers must not sweep and retry.
    ItemTooLarge,
    /// The owner's own byte budget is exhausted.
    ///
    /// Relieved when this owner releases something, so reclaiming within the
    /// owner is the correct response.
    PerOwnerBudget,
    /// The process-wide ceiling is reached across all owners.
    ///
    /// This owner may be well within its own budget. Reclaiming here helps
    /// only in proportion to what this owner holds.
    ProcessBudget,
    /// A per-owner count limit is reached, independent of bytes.
    ///
    /// Distinct from a byte budget because the remedy differs: releasing one
    /// large item relieves bytes but not a count.
    ItemCountLimit,
    /// Admission is closed because the owner is shutting down.
    Cancelled,
}

impl AdmissionRejection {
    /// Whether reclaiming and retrying could plausibly admit the item.
    ///
    /// [`Self::ItemTooLarge`] and [`Self::Cancelled`] are permanent for this
    /// item, so a caller that sweeps and retries on them burns a scan to reach
    /// the same answer. The hyperlink registry did exactly that before this
    /// distinction existed: an oversized URI triggered a full grid scan that
    /// could never change the outcome.
    #[must_use]
    pub fn is_retryable_after_reclaim(self) -> bool {
        match self {
            Self::PerOwnerBudget | Self::ProcessBudget | Self::ItemCountLimit => true,
            Self::ItemTooLarge | Self::Cancelled => false,
        }
    }

    /// Stable snake_case code for logs and tests.
    ///
    /// Stable so an operator can grep for it across versions, and so a
    /// diagnostic's meaning does not shift when a variant is renamed.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::ItemTooLarge => "item_too_large",
            Self::PerOwnerBudget => "per_owner_budget",
            Self::ProcessBudget => "process_budget",
            Self::ItemCountLimit => "item_count_limit",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Reasons delivery ownership may terminate at a connection boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum DisconnectReason {
    /// Sustained pressure exceeded the bounded retry policy.
    SustainedPressure,
    /// Underlying transport disconnected.
    TransportClosed,
    /// Lifecycle cancellation stopped admission.
    Cancelled,
}

/// Typed result of an operation governed by resource pressure.
#[derive(Debug)]
#[non_exhaustive]
pub enum PressureOutcome<T> {
    /// The receiver accepted ownership and produced a delivery receipt.
    Accepted {
        /// Accepted delivery identifier.
        receipt: DeliveryReceipt,
    },
    /// The receiver accepted ownership after deterministic eviction.
    Evicted {
        /// Accepted delivery identifier.
        receipt: DeliveryReceipt,
        /// Bytes released by eviction.
        released_bytes: usize,
    },
    /// Nothing was accepted; the caller retains `value` and retry responsibility.
    Backpressured {
        /// Caller-owned value.
        value: T,
        /// Retry scheduling information.
        retry: RetryToken,
    },
    /// A frozen policy allowed semantic loss for non-user data.
    Dropped {
        /// Number of dropped bytes.
        bytes: usize,
        /// Bounded policy reason.
        reason: DropReason,
    },
    /// Nothing was accepted; the caller retains `value`.
    Rejected {
        /// Caller-owned value.
        value: T,
        /// Resource-governor rejection.
        error: BudgetError,
    },
    /// Delivery disconnected, returning ownership where recovery is possible.
    Disconnected {
        /// Recoverable caller-owned value, when available.
        value: Option<T>,
        /// Bounded disconnect reason.
        reason: DisconnectReason,
    },
}

#[cfg(test)]
#[path = "resource_delivery_tests.rs"]
mod resource_delivery_tests;
