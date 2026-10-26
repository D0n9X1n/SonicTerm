use super::{
    BudgetDimension, BudgetError, BudgetScope, ChargeTransfer, EnumMap, Ledger, OwnerRecord,
    ResourceAmount, ResourceClass, ResourceOwnerId,
};
use std::sync::Arc;

struct OwnerTransferPlan {
    record: Arc<OwnerRecord>,
    removed: EnumMap<ResourceClass, ResourceAmount>,
    added: EnumMap<ResourceClass, ResourceAmount>,
}

impl Ledger {
    // Lock order: target states by owner id -> classes by enum -> usage by owner id; immutable parents always have smaller ids.
    pub(crate) fn transfer_many(&self, transfers: &[ChargeTransfer]) -> Result<(), BudgetError> {
        let mut plans = std::collections::BTreeMap::<ResourceOwnerId, OwnerTransferPlan>::new();
        let mut targets = std::collections::BTreeMap::<ResourceOwnerId, Arc<OwnerRecord>>::new();
        let mut class_totals: EnumMap<ResourceClass, ResourceAmount> = EnumMap::default();
        for transfer in transfers {
            let source_path = self.path(transfer.source)?;
            let target_path = self.path(transfer.target)?;
            for record in &target_path {
                targets.insert(record.id, record.clone());
            }
            if transfer.source == transfer.target {
                // When: source and target match, only open-target validation is owed; attribution stays unchanged.
                continue;
            }
            class_totals[transfer.class] =
                class_totals[transfer.class].checked_add(transfer.amount)?;
            for (path, adding) in [(&source_path, false), (&target_path, true)] {
                for record in Self::usage_records(path) {
                    let plan = plans.entry(record.id).or_insert_with(|| OwnerTransferPlan {
                        record: record.clone(),
                        removed: EnumMap::default(),
                        added: EnumMap::default(),
                    });
                    let amounts = if adding { &mut plan.added } else { &mut plan.removed };
                    amounts[transfer.class] =
                        amounts[transfer.class].checked_add(transfer.amount)?;
                }
            }
        }
        let targets: Vec<_> = targets.into_values().collect();
        let _states = Self::validate_state_path(&targets)?;
        let plans: Vec<_> = plans.into_values().collect();
        let classes: Vec<_> = class_totals
            .iter()
            .filter_map(|(class, amount)| (!amount.is_zero()).then_some(class))
            .collect();
        let class_guards: Vec<_> =
            classes.iter().map(|class| self.classes[*class].lock()).collect();
        let mut usages: Vec<_> = plans.iter().map(|plan| plan.record.usage.lock()).collect();
        for (class, usage) in classes.iter().zip(&class_guards) {
            if usage.bytes < class_totals[*class].bytes || usage.items < class_totals[*class].items
            {
                // When: process class balances cannot cover all moved tokens, reject before any owner changes.
                return Err(BudgetError::AccountingInvariant { owner: self.root, class: *class });
            }
        }
        let mut candidates = Vec::with_capacity(plans.len());
        for (plan, usage) in plans.iter().zip(&usages) {
            let mut candidate = (**usage).clone();
            let mut removed = ResourceAmount::default();
            let mut added = ResourceAmount::default();
            for class in &classes {
                let take = plan.removed[*class];
                let give = plan.added[*class];
                removed = removed.checked_add(take)?;
                added = added.checked_add(give)?;
                let base = ResourceAmount {
                    bytes: usage.class_bytes[*class],
                    items: usage.class_items[*class],
                }
                .checked_sub(take)
                .map_err(|_| BudgetError::AccountingInvariant {
                    owner: plan.record.id,
                    class: *class,
                })?;
                candidate.class_bytes[*class] = Self::validate_limit(
                    BudgetScope::OwnerClass { owner: plan.record.id, class: *class },
                    BudgetDimension::Bytes,
                    base.bytes,
                    give.bytes,
                    plan.record.limits.class_bytes[*class],
                )?;
                candidate.class_items[*class] = Self::validate_limit(
                    BudgetScope::OwnerClass { owner: plan.record.id, class: *class },
                    BudgetDimension::Items,
                    base.items,
                    give.items,
                    plan.record.limits.class_items[*class].unwrap_or(usize::MAX),
                )?;
            }
            let base = usage.amount.checked_sub(removed).map_err(|_| {
                BudgetError::AccountingInvariant {
                    owner: plan.record.id,
                    class: ResourceClass::RegistryMetadata,
                }
            })?;
            candidate.amount = ResourceAmount {
                bytes: Self::validate_limit(
                    BudgetScope::Owner(plan.record.id),
                    BudgetDimension::Bytes,
                    base.bytes,
                    added.bytes,
                    plan.record.limits.owner_bytes,
                )?,
                items: base.items.checked_add(added.items).ok_or(BudgetError::Overflow)?,
            };
            candidate.epoch = candidate.epoch.wrapping_add(1);
            candidates.push(candidate);
        }
        for (usage, candidate) in usages.iter_mut().zip(candidates) {
            **usage = candidate;
        }
        Ok(())
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
}

#[cfg(test)]
#[path = "ledger_transfer_tests.rs"]
mod ledger_transfer_tests;
