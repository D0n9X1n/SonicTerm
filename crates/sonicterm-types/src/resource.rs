//! Provider-neutral resource governance value types.
//!
//! Mutable accounting and reservation tokens live in `sonicterm-resource`.

use enum_map::{Array, Enum, EnumMap};
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
///
/// Adding a variant widens every `EnumMap` keyed by this type and breaks
/// exhaustive matches in every consuming crate, so the set is deliberately
/// complete rather than minimal: a class exists for each owner kind's
/// documented payload even where no subsystem charges it yet.
#[derive(Clone, Copy, Debug, Enum, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
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
    /// Parsed font faces, fallback tables, and shaping caches.
    ///
    /// Distinct from [`ResourceClass::GlyphRaster`]: this is the parsed input
    /// a rasterizer reads, retained for the life of the font stack, while
    /// raster output is transient and evictable.
    FontFace,
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

/// Why each [`ResourceClass`] is or is not charged in production.
///
/// A class with no charge site is indistinguishable, from the outside, from a
/// class someone forgot. This records which it is, with the measurement behind
/// the decision, and a test asserts the table covers the enum — so a new class
/// cannot be added without a decision being made about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassCoverage {
    /// A production site reserves and charges this class.
    Charged,
    /// The subsystem that would own it does not exist yet. Charging it would
    /// be charging nothing.
    SubsystemAbsent,
    /// Compiled out of shipped builds by a feature gate.
    FeatureGated,
    /// Allocated and released within one call, so a charge would be taken and
    /// returned before any sampler could observe it.
    TransientWithinCall,
    /// Real retention, measured, and small enough that charging it would cost
    /// more than it reports. The measurement is recorded so "small" is a
    /// finding rather than an assumption.
    MeasuredNegligible {
        /// Worst-case bytes retained per pane.
        per_pane_bytes: usize,
    },
    /// Real retention that nothing charges, recorded with what it holds.
    ///
    /// Distinct from every other variant, and the distinction is the point.
    /// [`Self::SubsystemAbsent`] says charging would charge nothing;
    /// [`Self::TransientWithinCall`] says the memory is gone before a sampler
    /// could see it; [`Self::MeasuredNegligible`] says it is too small to be
    /// worth reporting. This says the memory is present, significant, and
    /// outside the ledger — a gap with a number on it rather than a decision.
    ///
    /// A class recorded this way is work that has not been done. It exists so
    /// that fact is visible in the table instead of being spelled as one of
    /// the variants that mean "nothing to do here" — which is how a class with
    /// no charge site came to read `Charged`, and how a later derivation built
    /// on that row inherited the same claim.
    UnchargedRetention {
        /// Worst-case bytes retained by one owner of this class.
        per_owner_bytes: usize,
    },
}

impl ResourceClass {
    /// Number of variants in this enum.
    ///
    /// Derived from the enum-map array representation the `Enum` derive
    /// generates, so it cannot drift from the variant list: adding a variant
    /// widens the array and this constant follows. Callers that walk every
    /// class pair it with [`Enum::from_usize`].
    ///
    /// This exists because the class-coverage tests assert exhaustiveness by
    /// index rather than by matching, and [`ResourceClass`] is
    /// `#[non_exhaustive]` — a hardcoded count would silently stop covering
    /// the classes added after it was written, which is the failure those
    /// tests exist to catch.
    pub const COUNT: usize = <<Self as Enum>::Array<()> as Array>::LENGTH;

    /// This class's coverage decision.
    ///
    /// Exhaustive by construction: adding a variant to [`ResourceClass`] fails
    /// to compile here until someone decides what it is.
    #[must_use]
    pub const fn coverage(self) -> ClassCoverage {
        match self {
            // Charged from `sonicterm-app`'s retention sampling pass.
            Self::GridVisible
            | Self::GridHistory
            | Self::GridAlternate
            | Self::ParserCapture
            | Self::ProtocolMetadata
            | Self::InlineMediaRetained => ClassCoverage::Charged,

            // Reported but not charged, which is the distinction this variant
            // exists to carry. The app reads `retained_amounts` and emits a
            // `renderer retention` line per renderer, so these figures are
            // visible; what they do not reach is a ledger. `sonicterm-gpu`
            // declares no dependency on `sonicterm-resource`, so the crate that
            // computes them cannot reserve against a governor at all.
            //
            // `GlyphAtlas` is the atlas's own 2048 x 2048 x 4 pixel buffer.
            //
            // `SoftwareFrame` is the clamp, not a typical window.
            //
            // The buffer is `width x height x 4`, so no single figure describes
            // it, and `per_owner_bytes` asks how much can hide here rather than
            // what a session usually holds. The honest answer is the most one
            // surface may hold, which is `software_presenter::MAX_BYTES`.
            //
            // Measured against a counting allocator, per window:
            //
            //     1080p    8,294,400
            //     1440p   14,745,600
            //     4K      33,177,600
            //     5K      58,982,400
            //     8K     132,710,400
            //     clamp  167,772,160   <- this figure
            //
            // The clamp is structural rather than observed: `pixel_len` also
            // caps each axis at 16,384, so reaching it needs roughly
            // 6.5K x 6.5K — beyond one panel, reachable across a span. A 4K
            // window, the previous figure, understates the ceiling by 5.06x.
            //
            // `the_tabled_software_frame_bound_is_this_clamp` in
            // `sonicterm-windows` fails if the clamp moves without this.
            Self::GlyphAtlas => {
                ClassCoverage::UnchargedRetention { per_owner_bytes: 2048 * 2048 * 4 }
            }
            Self::SoftwareFrame => {
                ClassCoverage::UnchargedRetention { per_owner_bytes: 160 * 1024 * 1024 }
            }

            // PTY output queue: 64 slots of views into the reader's reused
            // 64 KiB ring. The charge is the ring memory those views pin, not
            // the slot count and not the payload — measured on a full queue
            // from `/bin/sh`, 64 bytes of keystroke echo pin 64 KiB, and a
            // sustained flood spans two rings at 128 KiB. The structural
            // ceiling is one ring per slot, 4 MiB, which needs reads large
            // enough to exhaust a ring apiece and no real shell reaches it.
            Self::PtyOutput => ClassCoverage::Charged,

            // PTY input queue: four slots, each a `Vec<u8>` accepted up to the
            // per-message cap. The slot count says nothing about the bytes
            // held — a paste is admitted at the full message size and
            // broadcast to every pane — so this is charged from an exact
            // count the queue maintains, not from a per-slot estimate.
            Self::PtyInput => ClassCoverage::Charged,

            // 64 bounded slots of DSR/XTVERSION replies. The replies are ~28
            // bytes, but the figure has to cover the channel's slot array and
            // each reply's `Vec` header too: measured at 4,480 bytes for a
            // full queue, against the 2,048 the payloads alone predict.
            Self::ParserReply => ClassCoverage::MeasuredNegligible { per_pane_bytes: 8 * 1024 },
            // 1024 bounded records of 40 bytes.
            Self::CommandEvents => ClassCoverage::MeasuredNegligible { per_pane_bytes: 40 * 1024 },

            // Decode buffers live inside `decode_inline_image`; the retained
            // result is charged as `InlineMediaRetained`.
            Self::InlineMediaDecode => ClassCoverage::TransientWithinCall,
            // Rasterized before atlas insertion, then owned by the atlas.
            Self::GlyphRaster => ClassCoverage::TransientWithinCall,

            // Upload staging is a field on `AtlasUpload`, cleared between
            // copies and never shrunk, so it holds the largest dirty rect it
            // has ever staged for as long as the renderer lives.
            //
            // **The reuse is deliberate and is not a defect to fix.** Copying
            // a rect per frame into a fresh allocation would trade this memory
            // for a per-frame allocation on the upload path;
            // `copies_tightly_packed_subrect_and_reuses_capacity` asserts the
            // capacity survives the call, and that assertion is the intended
            // behaviour rather than an accident this row is reporting.
            //
            // What is recorded here is only what the reuse costs. A dirty rect
            // cannot exceed the atlas it is copied from, so the ceiling is one
            // whole atlas — `ATLAS_DIM x ATLAS_DIM x BYTES_PER_PIXEL`, 16 MiB —
            // and a renderer holds two, one for glyphs and one for images.
            Self::UploadStaging => {
                ClassCoverage::UnchargedRetention { per_owner_bytes: 2 * 2048 * 2048 * 4 }
            }

            // SSH is `--features ssh`, off in shipped builds.
            Self::RemoteInput | Self::RemoteOutput => ClassCoverage::FeatureGated,

            // `sonicterm-mux` is server scaffolding with no live subscribers.
            Self::MuxSubscriber => ClassCoverage::SubsystemAbsent,
            // The reaper exists in `sonicterm-resource` and is not referenced
            // from `sonicterm-app`.
            Self::ReaperWork => ClassCoverage::SubsystemAbsent,
            // Owner records are the ledger's own storage. Charging them to a
            // ledger class would make the ledger account for itself, and the
            // recursion has no fixed point.
            Self::RegistryMetadata => ClassCoverage::SubsystemAbsent,

            // Parsed font data is retained for the life of the font stack and
            // shared across every pane, so it belongs to a `SharedFont` owner
            // that the app does not yet create.
            Self::FontFace => ClassCoverage::SubsystemAbsent,
            // GPU surface memory belongs to the driver; wgpu exposes no size
            // accounting for it, so any figure here would be a guess presented
            // as a measurement. `SoftwareFrame` covers the CPU-side buffer.
            Self::Surface => ClassCoverage::SubsystemAbsent,
        }
    }
}

/// Whether a class contributes a term to a pane owner's seam-cap sum.
///
/// The sum exists to derive a backstop above everything a pane owner can be
/// charged for. It is compared against that owner's ledger total, so it must
/// have a term for exactly the classes that can appear in that total — no more
/// and no fewer. A missing term puts the backstop below memory the seams
/// legitimately permit, where it fires during correct operation; a term for a
/// class that never charges a pane inflates it with memory that cannot appear
/// in the figure it guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneSeamTerm {
    /// A pane owner is charged for this class, so the sum carries its cap.
    Contributes,
    /// Charged, but to an owner other than a pane, so it never appears in a
    /// pane owner's total.
    ///
    /// **No class uses this today, and one wrongly did.** The renderer classes
    /// were recorded here on the reasoning that atlas memory belongs to the
    /// renderer's owner — sound, had anything charged it. Nothing does. Before
    /// putting a class here, name the owner kind whose ledger carries it and
    /// the site that charges it; if neither exists, the class belongs in
    /// [`Self::NotChargedInProduction`], which claims less and is checkable.
    ChargedToAnotherOwnerKind,
    /// No production site charges this class to any owner, so it cannot appear
    /// in a pane owner's total whatever it retains.
    NotChargedInProduction,
}

impl ResourceClass {
    /// This class's place in a pane owner's seam-cap sum.
    ///
    /// Exhaustive by construction, like [`ResourceClass::coverage`]: adding a
    /// variant fails to compile here until someone decides whether a pane owner
    /// can be charged for it. That is what keeps the sum a derivation rather
    /// than a figure that was once correct — a new charged class cannot reach
    /// production without either contributing a term or recording why it does
    /// not.
    #[must_use]
    pub const fn pane_seam_term(self) -> PaneSeamTerm {
        match self {
            // The three grid classes share one cap: `MAX_GRID_CELLS` bounds
            // visible, history, and saved-primary cells together rather than
            // each separately, so the sum carries that single term.
            Self::GridVisible | Self::GridHistory | Self::GridAlternate => {
                PaneSeamTerm::Contributes
            }

            // Capture staging and the escape sequence in flight, both bounded
            // by the parser's own caps.
            Self::ParserCapture => PaneSeamTerm::Contributes,
            // The hyperlink registry's retained strings.
            Self::ProtocolMetadata => PaneSeamTerm::Contributes,
            // Decoded pixels the pane keeps after rendering.
            Self::InlineMediaRetained => PaneSeamTerm::Contributes,
            // The reader ring the output queue pins. The sum carries the
            // structural ceiling of one ring per slot rather than the figure a
            // real shell reaches, because a backstop has to sit above what the
            // seam permits, not above what it usually uses.
            Self::PtyOutput => PaneSeamTerm::Contributes,

            // Atlas pixels and the software frame are charged to nobody. The
            // renderer computes both figures, but `retained_amounts` has no
            // caller and `sonicterm-gpu` cannot reserve, so they reach no
            // owner's ledger — a pane's least of all. The sum carries no term
            // for them for the same reason it carries none for the classes
            // below: not because the charge lands elsewhere, but because there
            // is no charge.
            Self::GlyphAtlas | Self::SoftwareFrame => PaneSeamTerm::NotChargedInProduction,

            // Charged to the pane that owns the queue, so it appears in that
            // pane's total and the backstop above it must carry its cap.
            Self::PtyInput => PaneSeamTerm::Contributes,

            // Real retention, bounded by its own seam, and small enough that
            // no charge site exists. Without a charge these cannot appear in a
            // pane's total, so the backstop that reads that total gains
            // nothing from carrying them.
            Self::ParserReply | Self::CommandEvents => PaneSeamTerm::NotChargedInProduction,

            // Released before the call that allocated them returns, so no
            // sampler sees them in a pane's total.
            Self::InlineMediaDecode | Self::GlyphRaster => PaneSeamTerm::NotChargedInProduction,

            // Retained rather than released, but retained by a renderer and
            // charged to nobody. A pane's ledger cannot carry it either way,
            // so the sum gains no term — for the same reason as the classes
            // above, though not for the same reason as their comment gives.
            Self::UploadStaging => PaneSeamTerm::NotChargedInProduction,

            // Compiled out of shipped builds.
            Self::RemoteInput | Self::RemoteOutput => PaneSeamTerm::NotChargedInProduction,

            // No subsystem charges these: mux has no live subscribers, the
            // reaper is unreferenced from the app, owner records are the
            // ledger's own storage, font faces await a shared owner, and GPU
            // surface memory has no size the driver exposes.
            Self::MuxSubscriber
            | Self::ReaperWork
            | Self::RegistryMetadata
            | Self::FontFace
            | Self::Surface => PaneSeamTerm::NotChargedInProduction,
        }
    }
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
            // When: `other` exceeds either component, subtraction would underflow that accounting axis.
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
#[non_exhaustive]
pub enum ProcessKind {
    /// GUI application process.
    Gui,
    /// Multiplexer daemon process.
    Mux,
}

/// Kind of node in the resource-owner hierarchy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
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
#[non_exhaustive]
pub enum BudgetDimension {
    /// Byte accounting dimension.
    Bytes,
    /// Item accounting dimension.
    Items,
}

/// Scope whose limit rejected an operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
pub enum ResizeOperation {
    /// New total must be component-wise no smaller.
    Grow,
    /// New total must be component-wise no larger.
    Shrink,
}

/// Observational owner and process accounting snapshot.
#[derive(Clone, Debug)]
#[non_exhaustive]
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
    /// Byte ceiling this owner is held to.
    ///
    /// Reported alongside the usage it bounds, because a usage figure without
    /// its limit is half a diagnostic: it says how much is held and not whether
    /// that is close to a problem. It is also the only way a caller can verify
    /// a limit was actually installed rather than merely computed — a limit
    /// nothing can read back is indistinguishable from a doc comment.
    pub owner_bytes_limit: usize,
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

/// One owner's view within a [`ResourceSnapshot`].
#[derive(Clone, Debug)]
pub struct OwnerView {
    /// Requested owner.
    pub owner: ResourceOwnerId,
    /// Requested owner's hierarchy kind.
    pub kind: OwnerKind,
    /// Requested owner's admission state.
    pub state: OwnerState,
    /// Parent owner, absent only for the process root.
    pub parent: Option<ResourceOwnerId>,
    /// Aggregate charge attributed to the owner.
    pub amount: ResourceAmount,
    /// Byte ceiling this owner is held to.
    pub bytes_limit: usize,
    /// Owner-local bytes by class.
    pub class_bytes: EnumMap<ResourceClass, usize>,
    /// Owner-local items by class.
    pub class_items: EnumMap<ResourceClass, usize>,
    /// Epoch observed with the owner fields.
    pub epoch: u64,
}

/// Process-wide totals within a [`ResourceSnapshot`].
#[derive(Clone, Debug)]
pub struct ProcessView {
    /// Aggregate process charge.
    pub amount: ResourceAmount,
    /// Process-wide bytes by class.
    pub class_bytes: EnumMap<ResourceClass, usize>,
    /// Process-wide items by class.
    pub class_items: EnumMap<ResourceClass, usize>,
    /// Independently observed epoch for each class shard.
    pub class_epochs: EnumMap<ResourceClass, u64>,
    /// Registry epoch observed by the snapshot.
    pub registry_epoch: u64,
    /// Accounting releases that could not be applied.
    pub release_failures: usize,
}

impl ResourceSnapshot {
    /// Assemble a snapshot from an owner view and the process totals.
    ///
    /// Grouping the fields keeps the call readable and lets the snapshot gain
    /// observations later without changing this signature, which is what the
    /// non-exhaustive marker is protecting: consumers read snapshots, only a
    /// governor produces them.
    pub fn new(process_kind: ProcessKind, owner: OwnerView, process: ProcessView) -> Self {
        Self {
            process_kind,
            owner: owner.owner,
            owner_kind: owner.kind,
            owner_state: owner.state,
            parent: owner.parent,
            owner_amount: owner.amount,
            owner_bytes_limit: owner.bytes_limit,
            owner_class_bytes: owner.class_bytes,
            owner_class_items: owner.class_items,
            process_amount: process.amount,
            process_class_bytes: process.class_bytes,
            process_class_items: process.class_items,
            owner_epoch: owner.epoch,
            class_epochs: process.class_epochs,
            registry_epoch: process.registry_epoch,
            release_failures: process.release_failures,
        }
    }
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
#[path = "resource_tests.rs"]
mod resource_tests;
