//! Provider-neutral lifecycle state and cancellation value types.
//!
//! The supervisor, reaper, and concrete transports live in `sonicterm-resource`
//! and the platform IO crates. This module owns only the state contract they
//! share, so a transport cannot invent its own teardown ordering.

use crate::{OwnerState, ResourceOwnerId};
use std::fmt;

/// Teardown state of a supervised transport or worker.
///
/// A resource moves forward only. Reaching [`LifecycleState::Closed`] means every
/// worker was joined or handed to the reaper, the native transport settled, child
/// owners closed, and the accounting ledger reached zero.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LifecycleState {
    /// Handles and worker ownership are being installed.
    Starting,
    /// Accepting external work.
    Running,
    /// Cancellation published; new external work is rejected.
    Cancelling,
    /// Closing or cancelling every blocking transport.
    ClosingTransport,
    /// Waiting for workers to join or transfer to the reaper.
    Joining,
    /// Process tree and native transport settled.
    Reaped,
    /// Fully settled, including a zero ledger.
    Closed,
    /// Failed with preserved error evidence.
    Faulted,
}

impl LifecycleState {
    /// Return whether this state still admits new external work.
    #[inline]
    pub const fn admits_work(self) -> bool {
        matches!(self, Self::Running)
    }

    /// Return whether this state is terminal.
    #[inline]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Closed)
    }

    /// Return whether a transition to `next` is permitted.
    ///
    /// A faulted resource never reaches [`LifecycleState::Closed`] directly: it
    /// either resumes normal teardown through [`LifecycleState::Cancelling`] or
    /// proves the same settlement preconditions and passes through
    /// [`LifecycleState::Reaped`]. Nothing returns to [`LifecycleState::Running`].
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Starting, Self::Running | Self::Cancelling | Self::Faulted)
                | (Self::Running, Self::Cancelling | Self::Faulted)
                | (Self::Cancelling, Self::ClosingTransport | Self::Faulted)
                | (Self::ClosingTransport, Self::Joining | Self::Faulted)
                | (Self::Joining, Self::Reaped | Self::Faulted)
                | (Self::Reaped, Self::Closed)
                | (Self::Faulted, Self::Cancelling | Self::Reaped)
        )
    }

    /// Return the action a resource must complete before entering this state.
    pub const fn entry_requirement(self) -> &'static str {
        match self {
            Self::Starting => "handles and worker ownership installed",
            Self::Running => "startup complete",
            Self::Cancelling => "reject new external work",
            Self::ClosingTransport => "publish level-triggered cancellation",
            Self::Joining => "close or cancel every blocking transport",
            Self::Reaped => "every worker joined or transferred to the reaper",
            Self::Closed => "process tree and ledger settled",
            Self::Faulted => "preserve owner and error evidence",
        }
    }

    /// Return the owner admission state this lifecycle state implies.
    ///
    /// The two enums answer different questions — this one tracks teardown
    /// progress, [`OwnerState`] tracks whether the ledger still admits work —
    /// so a subsystem holding both must not let them drift. A faulted resource
    /// maps to [`OwnerState::Closing`] rather than [`OwnerState::Closed`]
    /// because a fault does not settle the charges the owner still holds.
    pub const fn owner_state(self) -> OwnerState {
        match self {
            Self::Starting | Self::Running => OwnerState::Open,
            Self::Cancelling
            | Self::ClosingTransport
            | Self::Joining
            | Self::Reaped
            | Self::Faulted => OwnerState::Closing,
            Self::Closed => OwnerState::Closed,
        }
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::ClosingTransport => "closing-transport",
            Self::Joining => "joining",
            Self::Reaped => "reaped",
            Self::Closed => "closed",
            Self::Faulted => "faulted",
        };
        formatter.write_str(text)
    }
}

/// Rejected lifecycle transition.
///
/// Carrying both states keeps the illegal edge in telemetry rather than
/// collapsing every rejection into one opaque error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct IllegalTransition {
    /// Owner whose transition was rejected.
    pub owner: ResourceOwnerId,
    /// State the owner was in.
    pub from: LifecycleState,
    /// State the caller attempted to enter.
    pub to: LifecycleState,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "owner {} cannot transition from {} to {}",
            self.owner, self.from, self.to
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// Why a resource was asked to stop.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum CancelReason {
    /// Normal user- or app-initiated close.
    Requested,
    /// The owning parent is closing.
    ParentClosing,
    /// A deadline elapsed.
    Timeout,
    /// The process is shutting down.
    Shutdown,
    /// Cancellation follows a recorded fault.
    Faulted,
}

/// Outcome of asking a resource to stop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CancelOutcome {
    /// Stopped before the deadline.
    Settled,
    /// Still running; ownership stays with the caller or the reaper.
    Pending,
    /// The deadline elapsed. Ownership is retained, never dropped.
    TimedOut,
}

impl CancelOutcome {
    /// Return whether the resource is fully settled.
    ///
    /// A timeout is an outcome, not a release: unsettled work stays owned by the
    /// reaper and keeps its charge until terminal cleanup.
    #[inline]
    pub const fn is_settled(self) -> bool {
        matches!(self, Self::Settled)
    }
}

/// Terminal disposition of a reaped resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ReapResult {
    /// The resource settled and released everything it owned.
    Settled,
    /// Escalation terminated an owned helper process or OS resource.
    Escalated,
    /// The deadline elapsed. The charge stays with the original owner.
    TimedOut,
    /// Settlement failed and evidence is preserved for diagnosis.
    Failed,
}

impl ReapResult {
    /// Return whether the reaper may release its accounting for this task.
    ///
    /// Only a settled or escalated task has actually given up its resources.
    /// Timeouts and failures stay charged so a leak is visible rather than
    /// silently forgiven.
    #[inline]
    pub const fn releases_charge(self) -> bool {
        matches!(self, Self::Settled | Self::Escalated)
    }
}

/// Why a reaper refused to admit a task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ReapAdmission {
    /// A slot was reserved; ownership may transfer on enqueue.
    Reserved,
    /// No slot is available. The caller keeps ownership and must complete
    /// synchronously or retry; abandoning the resource is not permitted.
    QueueFull,
    /// The supervisor stopped admitting work.
    ShuttingDown,
}

impl ReapAdmission {
    /// Return whether a caller may hand ownership to the reaper.
    #[inline]
    pub const fn admits(self) -> bool {
        matches!(self, Self::Reserved)
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
