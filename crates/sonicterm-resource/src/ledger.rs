use crate::owner::{OwnerRecord, OwnerRegistry};
use enum_map::{enum_map, Enum, EnumMap};
use parking_lot::Mutex;
use sonicterm_types::{
    BudgetDimension, BudgetError, BudgetScope, GovernorLimits, OwnerKind, OwnerLimits, OwnerState,
    ProcessKind, ResourceAmount, ResourceClass, ResourceOwnerId, ResourceSnapshot,
};
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc,
};

pub(crate) struct ClassUsage {
    pub(crate) bytes: usize,
    pub(crate) items: usize,
    pub(crate) epoch: u64,
}

pub(crate) struct Ledger {
    pub(crate) process_kind: ProcessKind,
    pub(crate) limits: GovernorLimits,
    pub(crate) root: ResourceOwnerId,
    next_owner: AtomicU64,
    registry_epoch: AtomicU64,
    process_bytes: AtomicUsize,
    release_failures: AtomicUsize,
    pub(crate) registry: OwnerRegistry,
    classes: EnumMap<ResourceClass, Mutex<ClassUsage>>,
}

impl Ledger {
    pub(crate) fn new(kind: ProcessKind, limits: GovernorLimits) -> Result<Arc<Self>, BudgetError> {
        Self::new_with_next_id(kind, limits, 2)
    }

    pub(crate) fn new_with_next_id(
        kind: ProcessKind,
        limits: GovernorLimits,
        next_id: u64,
    ) -> Result<Arc<Self>, BudgetError> {
        let root = ResourceOwnerId::new(1).ok_or(BudgetError::OwnerIdExhausted)?;
        let registry = OwnerRegistry::new();
        let root_limits = OwnerLimits {
            owner_bytes: limits.process_bytes,
            class_bytes: limits.class_bytes,
            class_items: limits.class_items,
        };
        let root_record = Arc::new(OwnerRecord {
            id: root,
            kind: OwnerKind::Process,
            parent: None,
            limits: root_limits,
            state: parking_lot::RwLock::new(OwnerState::Open),
            usage: Mutex::new(crate::owner::OwnerUsage::open()),
        });
        registry.insert(root_record);
        Ok(Arc::new(Self {
            process_kind: kind,
            limits,
            root,
            next_owner: AtomicU64::new(next_id),
            registry_epoch: AtomicU64::new(1),
            process_bytes: AtomicUsize::new(0),
            release_failures: AtomicUsize::new(0),
            registry,
            classes: enum_map! { _ => Mutex::new(ClassUsage { bytes: 0, items: 0, epoch: 0 }) },
        }))
    }

    // Ordering: next_owner load/failure use Relaxed; compare_exchange_weak uses
    // AcqRel/Relaxed. The registry lock, not this counter, publishes records.
    pub(crate) fn allocate_owner_id(&self) -> Result<ResourceOwnerId, BudgetError> {
        let mut current = self.next_owner.load(Ordering::Relaxed);
        loop {
            if current == 0 || current == u64::MAX {
                // When: zero cannot form a non-zero owner id, and MAX cannot be
                // incremented without overflow; stop before panic or wrap to zero.
                return Err(BudgetError::OwnerIdExhausted);
            }
            match self.next_owner.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // When: compare_exchange_weak succeeds for current, so this
                    // caller exclusively owns the pre-increment candidate.
                    return ResourceOwnerId::new(current).ok_or(BudgetError::OwnerIdExhausted);
                }
                Err(observed) => current = observed,
            }
        }
    }

    fn child_kind_allowed(&self, parent: OwnerKind, child: OwnerKind) -> bool {
        matches!(
            (self.process_kind, parent, child),
            (
                ProcessKind::Gui,
                OwnerKind::Process,
                OwnerKind::SharedFont
                    | OwnerKind::SharedRaster
                    | OwnerKind::SharedAtlas
                    | OwnerKind::Window
                    // Retained model rule with no current producer: a client
                    // owns the connection it opens.
                    | OwnerKind::MuxConnection
            ) | (ProcessKind::Gui, OwnerKind::Window, OwnerKind::AppPane)
                | (ProcessKind::Gui, OwnerKind::AppPane, OwnerKind::LocalPty)
                | (ProcessKind::Gui, OwnerKind::MuxConnection, OwnerKind::Attachment)
                | (
                    ProcessKind::Mux,
                    OwnerKind::Process,
                    OwnerKind::MuxSession | OwnerKind::MuxConnection
                )
                | (ProcessKind::Mux, OwnerKind::MuxSession, OwnerKind::MuxPane)
                | (ProcessKind::Mux, OwnerKind::MuxPane, OwnerKind::PtyTransport)
                | (ProcessKind::Mux, OwnerKind::MuxConnection, OwnerKind::Attachment)
        )
    }

    // Lock order: registry lookup -> parent state -> parent usage -> registry
    // insertion; lookup releases its shard guard before owner locks overlap.
    // Ordering: registry_epoch fetch_add uses Release to announce hierarchy
    // change; the registry lock publishes record while usage remains locked.
    pub(crate) fn create_child(
        &self,
        parent_id: ResourceOwnerId,
        kind: OwnerKind,
        limits: OwnerLimits,
    ) -> Result<ResourceOwnerId, BudgetError> {
        let id = self.allocate_owner_id()?;
        let parent = self.registry.get(parent_id)?;
        if !self.child_kind_allowed(parent.kind, kind) {
            // When: an unlisted parent-child pair has no valid accounting and
            // close path, so reject it before child counts or registry state move.
            return Err(BudgetError::InvalidOwnerHierarchy {
                process: self.process_kind,
                parent: parent.kind,
                child: kind,
            });
        }
        let record = Arc::new(OwnerRecord {
            id,
            kind,
            parent: Some(parent.clone()),
            limits,
            state: parking_lot::RwLock::new(OwnerState::Open),
            usage: Mutex::new(crate::owner::OwnerUsage::open()),
        });
        let parent_state = parent.state.read();
        if *parent_state != OwnerState::Open {
            // When: parent_state is not Open; its held state guard linearizes
            // child rejection against concurrent close.
            return Err(BudgetError::InvalidOwnerState {
                owner: parent_id,
                expected: OwnerState::Open,
                actual: *parent_state,
            });
        }
        let mut parent_usage = parent.usage.lock();
        parent_usage.open_children =
            parent_usage.open_children.checked_add(1).ok_or(BudgetError::Overflow)?;
        parent_usage.epoch = parent_usage.epoch.wrapping_add(1);
        self.registry.insert(record);
        self.registry_epoch.fetch_add(1, Ordering::Release);
        Ok(id)
    }

    // Lock order: registry lookup -> owner state write -> owner usage; lookup
    // releases its shard guard before the two owner guards overlap.
    pub(crate) fn begin_close(&self, owner: ResourceOwnerId) -> Result<(), BudgetError> {
        let record = self.registry.get(owner)?;
        let mut state = record.state.write();
        if *state != OwnerState::Open {
            // When: only one caller may own the Open-to-Closing transition;
            // rejecting Closing also tells a competing closer it lost that role.
            return Err(BudgetError::InvalidOwnerState {
                owner,
                expected: OwnerState::Open,
                actual: *state,
            });
        }
        *state = OwnerState::Closing;
        let mut usage = record.usage.lock();
        usage.epoch = usage.epoch.wrapping_add(1);
        Ok(())
    }

    // Lock order: state -> usage -> class, with usage ordered by owner id; all
    // state/usage guards release before registry removal.
    // Ordering: process_bytes load uses Acquire; registry_epoch fetch_add uses
    // Release before removal. Root class/process samples span several instants.
    pub(crate) fn finish_close(&self, owner: ResourceOwnerId) -> Result<(), BudgetError> {
        let record = self.registry.get(owner)?;
        let mut state = record.state.write();
        if *state != OwnerState::Closing {
            // When: state is not Closing; finish requires admission shutdown and
            // rejects repeats that would decrement parent count twice.
            return Err(BudgetError::InvalidOwnerState {
                owner,
                expected: OwnerState::Closing,
                actual: *state,
            });
        }
        let mut records = if let Some(parent) = &record.parent {
            vec![parent.clone(), record.clone()]
        } else {
            // When: the process root has no parent usage or open-child count to
            // lock, so its guard set contains only itself.
            vec![record.clone()]
        };
        records.sort_by_key(|record| record.id);
        let mut guards: Vec<_> = records.iter().map(|record| record.usage.lock()).collect();
        let owner_index = records.iter().position(|candidate| candidate.id == owner).unwrap();
        let usage = &guards[owner_index];
        if usage.open_children != 0 {
            // When: usage open_children is nonzero; children close first so
            // retained charges and parent references cannot outlive this record.
            return Err(BudgetError::OwnerHasLiveChildren { owner, children: usage.open_children });
        }
        let live_amount = if owner == self.root {
            let items = self
                .classes
                .values()
                .try_fold(0usize, |total, class| total.checked_add(class.lock().items))
                .ok_or(BudgetError::Overflow)?;
            ResourceAmount { bytes: self.process_bytes.load(Ordering::Acquire), items }
        } else {
            // When: non-root usage already aggregates its subtree; only the root
            // is omitted from per-owner accounting and needs process totals.
            usage.amount
        };
        if !live_amount.is_zero() {
            // When: live_amount is not zero; tokens still resolve this record, so
            // it remains Closing until both accounting axes drain.
            return Err(BudgetError::OwnerHasLiveCharges { owner, amount: live_amount });
        }
        *state = OwnerState::Closed;
        guards[owner_index].epoch = guards[owner_index].epoch.wrapping_add(1);
        if let Some(parent) = &record.parent {
            let parent_index =
                records.iter().position(|candidate| candidate.id == parent.id).unwrap();
            guards[parent_index].open_children = guards[parent_index]
                .open_children
                .checked_sub(1)
                .ok_or(BudgetError::AccountingInvariant {
                    owner: parent.id,
                    class: ResourceClass::RegistryMetadata,
                })?;
            guards[parent_index].epoch = guards[parent_index].epoch.wrapping_add(1);
        }
        self.registry_epoch.fetch_add(1, Ordering::Release);
        drop(guards);
        drop(state);
        // The record is now unreferenced by the hierarchy, so drop it rather
        // than leaving a closed owner occupying its shard. Marking an owner
        // `Closed` returns no memory on its own: the record carries two
        // `EnumMap`s over every resource class plus two locks, measured at
        // roughly 1 KiB, and every tab or pane opened and closed adds one.
        //
        // The root is kept. It is the process owner, `root_owner` hands it out
        // for the life of the governor, and nothing closes it.
        if owner != self.root {
            self.registry.remove(owner);
        }
        Ok(())
    }

    fn path(&self, owner: ResourceOwnerId) -> Result<Vec<Arc<OwnerRecord>>, BudgetError> {
        Ok(OwnerRecord::path(&self.registry.get(owner)?))
    }

    fn validate_state_path(
        path: &[Arc<OwnerRecord>],
    ) -> Result<Vec<parking_lot::RwLockReadGuard<'_, OwnerState>>, BudgetError> {
        let states: Vec<_> = path.iter().map(|record| record.state.read()).collect();
        for (record, state) in path.iter().zip(states.iter()) {
            if **state != OwnerState::Open {
                // When: state is not Open; a closing ancestor stops subtree
                // admission and returned guards hold that decision through accounting.
                return Err(BudgetError::InvalidOwnerState {
                    owner: record.id,
                    expected: OwnerState::Open,
                    actual: **state,
                });
            }
        }
        Ok(states)
    }

    fn usage_records(path: &[Arc<OwnerRecord>]) -> &[Arc<OwnerRecord>] {
        if path.first().is_some_and(|record| record.kind == OwnerKind::Process) {
            &path[1..]
        } else {
            // When: a defensive non-rooted slice has no process prefix to strip;
            // preserving every record is safer than dropping a real owner.
            path
        }
    }

    fn validate_limit(
        scope: BudgetScope,
        dimension: BudgetDimension,
        current: usize,
        requested: usize,
        limit: usize,
    ) -> Result<usize, BudgetError> {
        let candidate = current.checked_add(requested).ok_or(BudgetError::Overflow)?;
        if candidate > limit {
            Err(BudgetError::LimitExceeded { scope, dimension, current, requested, limit })
        } else {
            // When: candidate does not exceed limit; the inclusive ceiling and
            // already-checked sum are returned for commit.
            Ok(candidate)
        }
    }

    // Lock order: classes -> usage; usage follows root-to-leaf owner ids. State
    // guards precede both; root usage is excluded to avoid inversion.
    // Ordering: process_bytes load uses Acquire; compare_exchange_weak uses
    // AcqRel/Acquire. Mutexes publish class and owner payloads.
    pub(crate) fn reserve(
        self: &Arc<Self>,
        owner: ResourceOwnerId,
        class: ResourceClass,
        amount: ResourceAmount,
    ) -> Result<(), BudgetError> {
        let path = self.path(owner)?;
        let _states = Self::validate_state_path(&path)?;
        let accounting_path = Self::usage_records(&path);
        let mut class_usage = self.classes[class].lock();
        let mut owner_usage: Vec<_> =
            accounting_path.iter().map(|record| record.usage.lock()).collect();
        for (record, usage) in accounting_path.iter().zip(owner_usage.iter()) {
            Self::validate_limit(
                BudgetScope::Owner(record.id),
                BudgetDimension::Bytes,
                usage.amount.bytes,
                amount.bytes,
                record.limits.owner_bytes,
            )?;
            usage.amount.items.checked_add(amount.items).ok_or(BudgetError::Overflow)?;
            Self::validate_limit(
                BudgetScope::OwnerClass { owner: record.id, class },
                BudgetDimension::Bytes,
                usage.class_bytes[class],
                amount.bytes,
                record.limits.class_bytes[class],
            )?;
            let class_items =
                usage.class_items[class].checked_add(amount.items).ok_or(BudgetError::Overflow)?;
            // When: record class_items Some limit caps class_items; exceeding limit
            // rejects before any owner, class, or process counter changes.
            if let Some(limit) = record.limits.class_items[class] {
                if class_items > limit {
                    // When: class_items exceeds limit after adding this request;
                    // reject before any accounting counter changes.
                    return Err(BudgetError::LimitExceeded {
                        scope: BudgetScope::OwnerClass { owner: record.id, class },
                        dimension: BudgetDimension::Items,
                        current: usage.class_items[class],
                        requested: amount.items,
                        limit,
                    });
                }
            }
        }
        Self::validate_limit(
            BudgetScope::ProcessClass(class),
            BudgetDimension::Bytes,
            class_usage.bytes,
            amount.bytes,
            self.limits.class_bytes[class],
        )?;
        let process_class_items =
            class_usage.items.checked_add(amount.items).ok_or(BudgetError::Overflow)?;
        // When: limits class_items Some limit spans owners; process_class_items
        // above limit is the final item rejection before later byte-only gates.
        if let Some(limit) = self.limits.class_items[class] {
            if process_class_items > limit {
                // When: process_class_items exceeds limit across owners; this is
                // the final item rejection before later byte-only gates.
                return Err(BudgetError::LimitExceeded {
                    scope: BudgetScope::ProcessClass(class),
                    dimension: BudgetDimension::Items,
                    current: class_usage.items,
                    requested: amount.items,
                    limit,
                });
            }
        }
        if amount.is_zero() {
            // When: amount is_zero after state and limit validation; no counter or
            // epoch changes, so no snapshot is invalidated.
            return Ok(());
        }
        // Only a byte charge moves the process total, so an items-only reservation
        // skips the shared atomic entirely. The skipped check cannot reject: the
        // process total is only ever written to an already validated value.
        if amount.bytes > 0 {
            // When: amount bytes is positive; process_bytes is the cross-class
            // serialization point, so items-only requests skip its contention.
            let mut process = self.process_bytes.load(Ordering::Acquire);
            loop {
                let candidate = Self::validate_limit(
                    BudgetScope::Process,
                    BudgetDimension::Bytes,
                    process,
                    amount.bytes,
                    self.limits.process_bytes,
                )?;
                match self.process_bytes.compare_exchange_weak(
                    process,
                    candidate,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        // When: this CAS owns the delta computed from the exact
                        // process total validated in this iteration.
                        break;
                    }
                    Err(observed) => process = observed,
                }
            }
        }
        class_usage.bytes += amount.bytes;
        class_usage.items = process_class_items;
        class_usage.epoch = class_usage.epoch.wrapping_add(1);
        for usage in &mut owner_usage {
            usage.amount = usage.amount.checked_add(amount).expect("prevalidated owner amount");
            usage.class_bytes[class] += amount.bytes;
            usage.class_items[class] = usage.class_items[class]
                .checked_add(amount.items)
                .expect("prevalidated owner class items");
            usage.epoch = usage.epoch.wrapping_add(1);
        }
        Ok(())
    }

    pub(crate) fn validate_open(&self, owner: ResourceOwnerId) -> Result<(), BudgetError> {
        let path = self.path(owner)?;
        let _states = Self::validate_state_path(&path)?;
        Ok(())
    }

    // Lock order: classes -> usage in root-to-leaf owner order. No state guard is
    // taken, so Closing owners can settle live tokens.
    // Ordering: process_bytes fetch_sub uses AcqRel after class and usage
    // reductions; mutexes publish the detailed accounting payload.
    pub(crate) fn release(
        &self,
        owner: ResourceOwnerId,
        class: ResourceClass,
        amount: ResourceAmount,
    ) -> Result<(), BudgetError> {
        if amount.is_zero() {
            // When: amount is_zero; skip owner lookup so a removed record cannot
            // turn a no-op into a false release-failure diagnostic.
            return Ok(());
        }
        let path = self.path(owner)?;
        let accounting_path = Self::usage_records(&path);
        let mut class_usage = self.classes[class].lock();
        let mut owner_usage: Vec<_> =
            accounting_path.iter().map(|record| record.usage.lock()).collect();
        if class_usage.bytes < amount.bytes || class_usage.items < amount.items {
            // When: class_usage bytes or items cannot cover amount; reject under
            // its guard before subtraction or mutation.
            return Err(BudgetError::AccountingInvariant { owner, class });
        }
        for usage in &owner_usage {
            if !amount.component_le(usage.amount)
                || usage.class_bytes[class] < amount.bytes
                || usage.class_items[class] < amount.items
            {
                // When: any aggregate or class bucket on the ancestor path is
                // short, reject the whole release before partial decrements diverge.
                return Err(BudgetError::AccountingInvariant { owner, class });
            }
        }
        class_usage.bytes -= amount.bytes;
        class_usage.items -= amount.items;
        class_usage.epoch = class_usage.epoch.wrapping_add(1);
        for usage in &mut owner_usage {
            usage.amount = usage.amount.checked_sub(amount).expect("prevalidated owner amount");
            usage.class_bytes[class] -= amount.bytes;
            usage.class_items[class] -= amount.items;
            usage.epoch = usage.epoch.wrapping_add(1);
        }
        self.process_bytes.fetch_sub(amount.bytes, Ordering::AcqRel);
        Ok(())
    }

    /// Record an accounting release that could not be applied.
    ///
    /// A failed release leaves the process ceiling permanently over-counted, so the
    /// count is exposed through snapshots to keep the violation observable in builds
    /// where debug assertions are compiled out.
    // Ordering: release_failures fetch_add uses conservative AcqRel; atomic RMW
    // prevents concurrent diagnostic increments from being lost.
    pub(crate) fn record_release_failure(&self) {
        self.release_failures.fetch_add(1, Ordering::AcqRel);
    }

    // Lock order: affected classes sorted by ResourceClass -> usage sorted by
    // owner id; target state precedes both, while source state stays unlocked.
    pub(crate) fn transfer_batch(
        &self,
        source_owner: ResourceOwnerId,
        target_owner: ResourceOwnerId,
        amounts: &EnumMap<ResourceClass, ResourceAmount>,
    ) -> Result<(), BudgetError> {
        let source_path = self.path(source_owner)?;
        let target_path = self.path(target_owner)?;
        let _target_states = Self::validate_state_path(&target_path)?;
        if source_owner == target_owner {
            // When: `source_owner == target_owner`, target-path validation is the
            // only effect required and every charge remains correctly attributed.
            return Ok(());
        }

        let affected_classes: Vec<_> = amounts
            .iter()
            .filter_map(|(class, amount)| (!amount.is_zero()).then_some(class))
            .collect();
        let total =
            affected_classes.iter().try_fold(ResourceAmount::default(), |total, class| {
                total.checked_add(amounts[*class])
            })?;
        let mut records: Vec<_> = source_path.iter().chain(target_path.iter()).cloned().collect();
        records.sort_by_key(|record| record.id);
        records.dedup_by_key(|record| record.id);
        let accounting_records = Self::usage_records(&records);
        let class_guards: Vec<_> =
            affected_classes.iter().map(|class| self.classes[*class].lock()).collect();
        let mut owner_guards: Vec<_> =
            accounting_records.iter().map(|record| record.usage.lock()).collect();
        let source_ids: std::collections::HashSet<_> =
            source_path.iter().map(|record| record.id).collect();
        let target_ids: std::collections::HashSet<_> =
            target_path.iter().map(|record| record.id).collect();
        let invariant_class =
            affected_classes.first().copied().unwrap_or(ResourceClass::RegistryMetadata);

        for (record, usage) in accounting_records.iter().zip(owner_guards.iter()) {
            let in_source = source_ids.contains(&record.id);
            let in_target = target_ids.contains(&record.id);
            if in_source && !in_target && !total.component_le(usage.amount) {
                // When: `in_source && !in_target` and `total` exceeds `usage.amount`,
                // reject before any class or owner shard is changed.
                return Err(BudgetError::AccountingInvariant {
                    owner: source_owner,
                    class: invariant_class,
                });
            }
            if in_target && !in_source {
                Self::validate_limit(
                    BudgetScope::Owner(record.id),
                    BudgetDimension::Bytes,
                    usage.amount.bytes,
                    total.bytes,
                    record.limits.owner_bytes,
                )?;
                usage.amount.items.checked_add(total.items).ok_or(BudgetError::Overflow)?;
            }

            for class in &affected_classes {
                let amount = amounts[*class];
                let source_bytes = if in_source { amount.bytes } else { 0 };
                let source_items = if in_source { amount.items } else { 0 };
                let target_bytes = if in_target { amount.bytes } else { 0 };
                let target_items = if in_target { amount.items } else { 0 };
                let bytes_after_source =
                    usage.class_bytes[*class].checked_sub(source_bytes).ok_or(
                        BudgetError::AccountingInvariant { owner: source_owner, class: *class },
                    )?;
                let items_after_source =
                    usage.class_items[*class].checked_sub(source_items).ok_or(
                        BudgetError::AccountingInvariant { owner: source_owner, class: *class },
                    )?;
                if in_target {
                    // When: `in_target`, validate the class after subtracting any
                    // shared source-path amount and before mutating either path.
                    Self::validate_limit(
                        BudgetScope::OwnerClass { owner: record.id, class: *class },
                        BudgetDimension::Bytes,
                        bytes_after_source,
                        target_bytes,
                        record.limits.class_bytes[*class],
                    )?;
                    let target_item_total = items_after_source
                        .checked_add(target_items)
                        .ok_or(BudgetError::Overflow)?;
                    if let Some(limit) = record.limits.class_items[*class] {
                        // When: `record.limits.class_items[*class]` is `Some(limit)`,
                        // enforce that optional item ceiling for this target path.
                        if target_item_total > limit {
                            // When: `target_item_total > limit`, reject while all
                            // guards still protect unchanged shards.
                            return Err(BudgetError::LimitExceeded {
                                scope: BudgetScope::OwnerClass { owner: record.id, class: *class },
                                dimension: BudgetDimension::Items,
                                current: items_after_source,
                                requested: target_items,
                                limit,
                            });
                        }
                    }
                }
            }
        }

        for (class, usage) in affected_classes.iter().zip(class_guards.iter()) {
            let amount = amounts[*class];
            if usage.bytes < amount.bytes || usage.items < amount.items {
                // When: `usage.bytes < amount.bytes` or `usage.items < amount.items`,
                // reject before owner attribution is modified.
                return Err(BudgetError::AccountingInvariant {
                    owner: source_owner,
                    class: *class,
                });
            }
        }

        for (record, usage) in accounting_records.iter().zip(owner_guards.iter_mut()) {
            let in_source = source_ids.contains(&record.id);
            let in_target = target_ids.contains(&record.id);
            for class in &affected_classes {
                let amount = amounts[*class];
                if in_source {
                    usage.class_bytes[*class] -= amount.bytes;
                    usage.class_items[*class] -= amount.items;
                }
                if in_target {
                    usage.class_bytes[*class] += amount.bytes;
                    usage.class_items[*class] += amount.items;
                }
            }
            if in_source && !in_target {
                usage.amount =
                    usage.amount.checked_sub(total).expect("prevalidated batch source amount");
            }
            if in_target && !in_source {
                usage.amount =
                    usage.amount.checked_add(total).expect("prevalidated batch target amount");
            }
            if in_source || in_target {
                usage.epoch = usage.epoch.wrapping_add(1);
            }
        }
        Ok(())
    }

    // Lock order: classes sorted by ResourceClass -> usage sorted by owner id;
    // target state precedes both, while source state stays unlocked for transfer-out.
    pub(crate) fn transfer(
        &self,
        source_owner: ResourceOwnerId,
        source_class: ResourceClass,
        target_owner: ResourceOwnerId,
        target_class: ResourceClass,
        amount: ResourceAmount,
    ) -> Result<(), BudgetError> {
        let source_path = self.path(source_owner)?;
        let target_path = self.path(target_owner)?;
        if amount.is_zero() {
            // When: amount is_zero; source/target identity and target state still
            // validate so an empty token cannot enter a Closing subtree.
            let _states = Self::validate_state_path(&target_path)?;
            return Ok(());
        }
        if source_owner == target_owner && source_class == target_class {
            // When: source_owner/source_class equal target_owner/target_class;
            // validate Open without subtracting, re-adding, or bumping epochs.
            return self.validate_open(target_owner);
        }
        let mut classes = vec![source_class];
        if target_class != source_class {
            classes.push(target_class);
            classes.sort();
        }
        let mut records: Vec<_> = source_path.iter().chain(target_path.iter()).cloned().collect();
        records.sort_by_key(|record| record.id);
        records.dedup_by_key(|record| record.id);
        let _target_states = Self::validate_state_path(&target_path)?;
        let accounting_records = Self::usage_records(&records);
        let mut class_guards: Vec<_> =
            classes.iter().map(|class| self.classes[*class].lock()).collect();
        let mut owner_guards: Vec<_> =
            accounting_records.iter().map(|record| record.usage.lock()).collect();
        let source_ids: std::collections::HashSet<_> =
            source_path.iter().map(|record| record.id).collect();
        let target_ids: std::collections::HashSet<_> =
            target_path.iter().map(|record| record.id).collect();
        for (record, usage) in accounting_records.iter().zip(owner_guards.iter()) {
            let source_only = source_ids.contains(&record.id) && !target_ids.contains(&record.id);
            let target_only = target_ids.contains(&record.id) && !source_ids.contains(&record.id);
            if source_only && !amount.component_le(usage.amount) {
                // When: only source-exclusive ancestors lose aggregate usage;
                // shared ancestors retain the charge while its attribution moves.
                return Err(BudgetError::AccountingInvariant {
                    owner: source_owner,
                    class: source_class,
                });
            }
            if target_only {
                Self::validate_limit(
                    BudgetScope::Owner(record.id),
                    BudgetDimension::Bytes,
                    usage.amount.bytes,
                    amount.bytes,
                    record.limits.owner_bytes,
                )?;
                usage.amount.items.checked_add(amount.items).ok_or(BudgetError::Overflow)?;
            }
            let source_class_bytes = if source_ids.contains(&record.id) { amount.bytes } else { 0 };
            let source_class_items = if source_ids.contains(&record.id) { amount.items } else { 0 };
            let target_class_bytes = if target_ids.contains(&record.id) { amount.bytes } else { 0 };
            let target_class_items = if target_ids.contains(&record.id) { amount.items } else { 0 };
            let bytes_after_source =
                usage.class_bytes[source_class].checked_sub(source_class_bytes).ok_or(
                    BudgetError::AccountingInvariant { owner: source_owner, class: source_class },
                )?;
            let items_after_source =
                usage.class_items[source_class].checked_sub(source_class_items).ok_or(
                    BudgetError::AccountingInvariant { owner: source_owner, class: source_class },
                )?;
            let target_bytes_current = if target_class == source_class {
                bytes_after_source
            } else {
                usage.class_bytes[target_class]
            };
            let target_items_current = if target_class == source_class {
                items_after_source
            } else {
                usage.class_items[target_class]
            };
            target_bytes_current.checked_add(target_class_bytes).ok_or(BudgetError::Overflow)?;
            target_items_current.checked_add(target_class_items).ok_or(BudgetError::Overflow)?;
            Self::validate_limit(
                BudgetScope::OwnerClass { owner: record.id, class: target_class },
                BudgetDimension::Bytes,
                target_bytes_current,
                target_class_bytes,
                record.limits.class_bytes[target_class],
            )?;
            if let Some(limit) = record.limits.class_items[target_class] {
                Self::validate_limit(
                    BudgetScope::OwnerClass { owner: record.id, class: target_class },
                    BudgetDimension::Items,
                    target_items_current,
                    target_class_items,
                    limit,
                )?;
            }
        }
        let source_index = classes.iter().position(|class| *class == source_class).unwrap();
        let target_index = classes.iter().position(|class| *class == target_class).unwrap();
        if class_guards[source_index].bytes < amount.bytes
            || class_guards[source_index].items < amount.items
        {
            // When: class_guards source_index cannot cover amount bytes/items;
            // process shards need independent validation before mutation.
            return Err(BudgetError::AccountingInvariant {
                owner: source_owner,
                class: source_class,
            });
        }
        let target_class_bytes = class_guards[target_index].bytes
            - if source_class == target_class { amount.bytes } else { 0 };
        let target_class_items = class_guards[target_index].items
            - if source_class == target_class { amount.items } else { 0 };
        target_class_items.checked_add(amount.items).ok_or(BudgetError::Overflow)?;
        Self::validate_limit(
            BudgetScope::ProcessClass(target_class),
            BudgetDimension::Bytes,
            target_class_bytes,
            amount.bytes,
            self.limits.class_bytes[target_class],
        )?;
        if let Some(limit) = self.limits.class_items[target_class] {
            Self::validate_limit(
                BudgetScope::ProcessClass(target_class),
                BudgetDimension::Items,
                target_class_items,
                amount.items,
                limit,
            )?;
        }
        class_guards[source_index].bytes -= amount.bytes;
        class_guards[source_index].items -= amount.items;
        class_guards[source_index].epoch = class_guards[source_index].epoch.wrapping_add(1);
        class_guards[target_index].bytes += amount.bytes;
        class_guards[target_index].items += amount.items;
        class_guards[target_index].epoch = class_guards[target_index].epoch.wrapping_add(1);
        for (record, usage) in accounting_records.iter().zip(owner_guards.iter_mut()) {
            let in_source = source_ids.contains(&record.id);
            let in_target = target_ids.contains(&record.id);
            if in_source {
                usage.class_bytes[source_class] -= amount.bytes;
                usage.class_items[source_class] -= amount.items;
                if !in_target {
                    usage.amount = usage
                        .amount
                        .checked_sub(amount)
                        .expect("prevalidated transfer source amount");
                }
            }
            if in_target {
                usage.class_bytes[target_class] += amount.bytes;
                usage.class_items[target_class] += amount.items;
                if !in_source {
                    usage.amount = usage
                        .amount
                        .checked_add(amount)
                        .expect("prevalidated transfer target amount");
                }
            }
            if in_source || in_target {
                usage.epoch = usage.epoch.wrapping_add(1);
            }
        }
        Ok(())
    }

    // Lock order: state -> usage -> classes in enum order; each guard releases
    // before the next, trading one instant for deadlock-free shard samples.
    // Ordering: registry_epoch and release_failures loads use Acquire as
    // independent samples; they do not make prior shard reads transactional.
    pub(crate) fn snapshot(&self, owner: ResourceOwnerId) -> Result<ResourceSnapshot, BudgetError> {
        let record = self.registry.get(owner)?;
        let owner_state = *record.state.read();
        let usage = record.usage.lock().clone();
        let mut process_class_bytes = EnumMap::default();
        let mut process_class_items = EnumMap::default();
        let mut class_epochs = EnumMap::default();
        for index in 0..ResourceClass::COUNT {
            let class = ResourceClass::from_usize(index);
            let class_usage = self.classes[class].lock();
            process_class_bytes[class] = class_usage.bytes;
            process_class_items[class] = class_usage.items;
            class_epochs[class] = class_usage.epoch;
        }
        let process_items = process_class_items
            .values()
            .try_fold(0usize, |total, items| total.checked_add(*items))
            .ok_or(BudgetError::Overflow)?;
        // Both axes are summed from the class shards observed above, so a
        // snapshot agrees with itself. Reading the process byte atomic here
        // instead would mix two instants into one observation, and a reader
        // could not tell a real imbalance from the sampling skew.
        let process_bytes = process_class_bytes
            .values()
            .try_fold(0usize, |total, bytes| total.checked_add(*bytes))
            .ok_or(BudgetError::Overflow)?;
        let process_amount = ResourceAmount { bytes: process_bytes, items: process_items };
        let (owner_amount, owner_class_bytes, owner_class_items) = if owner == self.root {
            (process_amount, process_class_bytes, process_class_items)
        } else {
            // When: root alone is excluded from per-owner accounting and uses
            // process totals; every other subtree aggregate was cloned under its lock.
            (usage.amount, usage.class_bytes, usage.class_items)
        };
        Ok(ResourceSnapshot::new(
            self.process_kind,
            sonicterm_types::OwnerView {
                owner,
                kind: record.kind,
                state: owner_state,
                parent: record.parent.as_ref().map(|parent| parent.id),
                amount: owner_amount,
                bytes_limit: record.limits.owner_bytes,
                class_bytes: owner_class_bytes,
                class_items: owner_class_items,
                epoch: usage.epoch,
            },
            sonicterm_types::ProcessView {
                amount: process_amount,
                class_bytes: process_class_bytes,
                class_items: process_class_items,
                class_epochs,
                registry_epoch: self.registry_epoch.load(Ordering::Acquire),
                release_failures: self.release_failures.load(Ordering::Acquire),
            },
        ))
    }
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod ledger_tests;
