//! Provider-neutral resource governance value types.
//!
//! Mutable accounting and reservation tokens live in `sonicterm-resource`.

use enum_map::{Enum, EnumMap};
use std::{fmt, num::NonZeroU64, time::Instant};

/// Stable in-process identity for a resource owner.
///
/// IDs are allocated monotonically by a resource governor and are never reused.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceOwnerId(NonZeroU64);

impl ResourceOwnerId {
    /// Construct an owner ID, returning `None` for zero.
    #[inline]
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Return the wrapped nonzero integer.
    #[inline]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for ResourceOwnerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Resource classes governed by the shared accounting contract.
#[derive(Clone, Copy, Debug, Enum, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResourceClass {
    /// Cells and associated storage in the visible primary grid.
    GridVisible,
    /// Retained scrollback history.
    GridHistory,
    /// Alternate-screen storage.
    GridAlternate,
    /// GPU or native presentation surface storage.
    Surface,
    /// CPU software-rendering frame storage.
    SoftwareFrame,
    /// CPU/GPU upload staging storage.
    UploadStaging,
    /// Rasterized glyph storage before atlas insertion.
    GlyphRaster,
    /// Glyph-atlas pixels and identity metadata.
    GlyphAtlas,
    /// VT escape and media capture storage.
    ParserCapture,
    /// Transient inline-media decoding storage.
    InlineMediaDecode,
    /// Retained decoded inline-media storage.
    InlineMediaRetained,
    /// Local PTY output queued or in flight.
    PtyOutput,
    /// Local PTY input queued or in flight.
    PtyInput,
    /// Parser-generated replies awaiting delivery.
    ParserReply,
    /// Remote-session input queued or in flight.
    RemoteInput,
    /// Remote-session output queued or in flight.
    RemoteOutput,
    /// Retained protocol metadata such as hyperlinks and prompts.
    ProtocolMetadata,
    /// Per-subscriber mux queue storage.
    MuxSubscriber,
    /// Retained command-event records.
    CommandEvents,
    /// Window, pane, and native registration metadata.
    RegistryMetadata,
    /// Work and native handles owned by the bounded reaper.
    ReaperWork,
}

/// Two-dimensional amount charged to a resource class.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResourceAmount {
    /// Retained or transient bytes.
    pub bytes: usize,
    /// Independently retained records, messages, tasks, or handles.
    pub items: usize,
}

impl ResourceAmount {
    /// Add both dimensions with overflow checking.
    pub fn checked_add(self, other: Self) -> Result<Self, BudgetError> {
        Ok(Self {
            bytes: self.bytes.checked_add(other.bytes).ok_or(BudgetError::Overflow)?,
            items: self.items.checked_add(other.items).ok_or(BudgetError::Overflow)?,
        })
    }

    /// Subtract both dimensions, rejecting component-wise underflow.
    pub fn checked_sub(self, other: Self) -> Result<Self, BudgetError> {
        if !other.component_le(self) {
            return Err(BudgetError::AmountExceedsCharge { requested: other, available: self });
        }
        Ok(Self { bytes: self.bytes - other.bytes, items: self.items - other.items })
    }

    /// Return whether both dimensions are no larger than `other`.
    #[inline]
    pub const fn component_le(self, other: Self) -> bool {
        self.bytes <= other.bytes && self.items <= other.items
    }

    /// Return whether both dimensions are zero.
    #[inline]
    pub const fn is_zero(self) -> bool {
        self.bytes == 0 && self.items == 0
    }
}

/// Immutable process-wide resource limits.
#[derive(Clone, Debug)]
pub struct GovernorLimits {
    /// Aggregate byte ceiling for the process.
    pub process_bytes: usize,
    /// Process-wide byte ceiling for each class.
    pub class_bytes: EnumMap<ResourceClass, usize>,
    /// Optional process-wide item ceiling for each class.
    pub class_items: EnumMap<ResourceClass, Option<usize>>,
}

/// Immutable limits copied into one owner record.
#[derive(Clone, Debug)]
pub struct OwnerLimits {
    /// Aggregate byte ceiling for the owner across all classes.
    pub owner_bytes: usize,
    /// Owner-local byte ceiling for each class.
    pub class_bytes: EnumMap<ResourceClass, usize>,
    /// Optional owner-local item ceiling for each class.
    pub class_items: EnumMap<ResourceClass, Option<usize>>,
}

/// Kind of process owning one independent resource governor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProcessKind {
    /// GUI application process.
    Gui,
    /// Multiplexer daemon process.
    Mux,
}

/// Kind of node in the resource-owner hierarchy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OwnerKind {
    /// Process root owner.
    Process,
    /// Shared font-discovery and parsed-font storage.
    SharedFont,
    /// Shared glyph-raster storage.
    SharedRaster,
    /// Shared atlas storage.
    SharedAtlas,
    /// GUI window owner.
    Window,
    /// GUI pane owner.
    AppPane,
    /// Locally owned PTY below a GUI pane.
    LocalPty,
    /// Persistent mux session owner.
    MuxSession,
    /// Persistent mux pane owner.
    MuxPane,
    /// PTY transport owned by a mux pane.
    PtyTransport,
    /// Mux client connection owner.
    MuxConnection,
    /// Connection attachment owner.
    Attachment,
}

/// Admission state of a resource owner.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OwnerState {
    /// New reservations and children are accepted.
    Open,
    /// Admission has stopped while owned resources are released.
    Closing,
    /// The owner has no children or charges and is terminal.
    Closed,
}

/// Dimension involved in a limit failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BudgetDimension {
    /// Byte accounting dimension.
    Bytes,
    /// Item accounting dimension.
    Items,
}

/// Scope whose limit rejected an operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BudgetScope {
    /// Aggregate process byte limit.
    Process,
    /// Process-wide limit for one class.
    ProcessClass(ResourceClass),
    /// Aggregate limit for one owner.
    Owner(ResourceOwnerId),
    /// Per-class limit for one owner.
    OwnerClass {
        /// Owner whose class limit rejected the request.
        owner: ResourceOwnerId,
        /// Rejected resource class.
        class: ResourceClass,
    },
}

/// Reasons a resource-governor operation can fail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BudgetError {
    /// No further nonzero owner ID can be allocated.
    OwnerIdExhausted,
    /// Checked resource arithmetic overflowed.
    Overflow,
    /// The requested owner ID is not registered.
    OwnerNotFound(ResourceOwnerId),
    /// The owner was not in the state required by the operation.
    InvalidOwnerState {
        /// Owner whose state was rejected.
        owner: ResourceOwnerId,
        /// State required by the operation.
        expected: OwnerState,
        /// State observed by the operation.
        actual: OwnerState,
    },
    /// The requested parent/child kinds do not belong to this process hierarchy.
    InvalidOwnerHierarchy {
        /// Process whose owner tree was being modified.
        process: ProcessKind,
        /// Existing parent owner kind.
        parent: OwnerKind,
        /// Rejected child owner kind.
        child: OwnerKind,
    },
    /// An owner cannot close while children remain open.
    OwnerHasLiveChildren {
        /// Owner being closed.
        owner: ResourceOwnerId,
        /// Number of open children.
        children: usize,
    },
    /// An owner cannot close while charges remain live.
    OwnerHasLiveCharges {
        /// Owner being closed.
        owner: ResourceOwnerId,
        /// Aggregate live charge.
        amount: ResourceAmount,
    },
    /// A process, class, owner, or owner-class limit was exceeded.
    LimitExceeded {
        /// Rejected limit scope.
        scope: BudgetScope,
        /// Rejected accounting dimension.
        dimension: BudgetDimension,
        /// Current value before the request.
        current: usize,
        /// Additional value requested.
        requested: usize,
        /// Immutable configured limit.
        limit: usize,
    },
    /// A split, commit, or subtraction requested more than the live charge.
    AmountExceedsCharge {
        /// Requested amount.
        requested: ResourceAmount,
        /// Available amount.
        available: ResourceAmount,
    },
    /// A resize operation moved in the wrong component-wise direction.
    InvalidResize {
        /// Operation whose direction was rejected.
        operation: ResizeOperation,
        /// Current committed amount.
        current: ResourceAmount,
        /// Requested new total.
        requested: ResourceAmount,
    },
    /// An internal release would underflow recorded accounting.
    AccountingInvariant {
        /// Owner whose recorded accounting was inconsistent.
        owner: ResourceOwnerId,
        /// Resource class whose accounting was inconsistent.
        class: ResourceClass,
    },
}

impl fmt::Display for BudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for BudgetError {}

/// Direction required by an in-place committed resize.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResizeOperation {
    /// New total must be component-wise no smaller.
    Grow,
    /// New total must be component-wise no larger.
    Shrink,
}

/// Observational owner and process accounting snapshot.
#[derive(Clone, Debug)]
pub struct ResourceSnapshot {
    /// Process kind for this ledger.
    pub process_kind: ProcessKind,
    /// Requested owner.
    pub owner: ResourceOwnerId,
    /// Requested owner's hierarchy kind.
    pub owner_kind: OwnerKind,
    /// Requested owner's admission state.
    pub owner_state: OwnerState,
    /// Parent owner, absent only for the process root.
    pub parent: Option<ResourceOwnerId>,
    /// Aggregate charge attributed to the owner.
    pub owner_amount: ResourceAmount,
    /// Owner-local bytes by class.
    pub owner_class_bytes: EnumMap<ResourceClass, usize>,
    /// Owner-local items by class.
    pub owner_class_items: EnumMap<ResourceClass, usize>,
    /// Aggregate process charge.
    pub process_amount: ResourceAmount,
    /// Process-wide bytes by class.
    pub process_class_bytes: EnumMap<ResourceClass, usize>,
    /// Process-wide items by class.
    pub process_class_items: EnumMap<ResourceClass, usize>,
    /// Epoch observed with the owner fields.
    pub owner_epoch: u64,
    /// Independently observed epoch for each class shard.
    pub class_epochs: EnumMap<ResourceClass, u64>,
    /// Registry epoch observed by the snapshot.
    pub registry_epoch: u64,
    /// Accounting releases that could not be applied since governor creation.
    ///
    /// Any non-zero value means the process ceiling is permanently over-counted and
    /// an owner cannot reach zero. Always zero in a consistent ledger.
    pub release_failures: usize,
}

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
pub enum DropReason {
    /// Frozen pressure policy explicitly permits dropping this class.
    Policy,
    /// Superseded coalescible state was replaced by newer state.
    Superseded,
}

/// Reasons delivery ownership may terminate at a connection boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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
#[path = "resource_tests.rs"]
mod resource_tests;
