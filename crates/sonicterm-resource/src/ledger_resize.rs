use super::{
    BudgetDimension, BudgetError, BudgetScope, Ledger, ResourceAmount, ResourceClass,
    ResourceOwnerId,
};
use std::sync::atomic::Ordering;

impl Ledger {
    // Lock order: validate_state_path -> classes[class] -> record.usage; immutable parents have smaller owner ids.
    // Ordering: process_bytes CAS uses AcqRel/Acquire before shard writes; net shrink uses AcqRel after validation.
    pub(crate) fn resize(
        &self,
        owner: ResourceOwnerId,
        class: ResourceClass,
        current: ResourceAmount,
        actual: ResourceAmount,
    ) -> Result<(), BudgetError> {
        if current == actual {
            // When: `current == actual`, no admission or accounting change is needed, including during close.
            return Ok(());
        }
        let path = self.path(owner)?;
        let _states = if actual.component_le(current) {
            Vec::new()
        } else {
            // When: any `actual` axis grows, keep every ancestor open until the resize commits.
            Self::validate_state_path(&path)?
        };
        let accounting_path = Self::usage_records(&path);
        let mut class_usage = self.classes[class].lock();
        let mut owner_usage: Vec<_> =
            accounting_path.iter().map(|record| record.usage.lock()).collect();
        let invariant = || BudgetError::AccountingInvariant { owner, class };
        let class_base = ResourceAmount { bytes: class_usage.bytes, items: class_usage.items }
            .checked_sub(current)
            .map_err(|_| invariant())?;
        let class_next = ResourceAmount {
            bytes: Self::validate_limit(
                BudgetScope::ProcessClass(class),
                BudgetDimension::Bytes,
                class_base.bytes,
                actual.bytes,
                self.limits.class_bytes[class],
            )?,
            items: Self::validate_limit(
                BudgetScope::ProcessClass(class),
                BudgetDimension::Items,
                class_base.items,
                actual.items,
                self.limits.class_items[class].unwrap_or(usize::MAX),
            )?,
        };
        let mut owner_next = Vec::with_capacity(owner_usage.len());
        for (record, usage) in accounting_path.iter().zip(&owner_usage) {
            let base = usage.amount.checked_sub(current).map_err(|_| invariant())?;
            let class_base =
                ResourceAmount { bytes: usage.class_bytes[class], items: usage.class_items[class] }
                    .checked_sub(current)
                    .map_err(|_| invariant())?;
            let amount = ResourceAmount {
                bytes: Self::validate_limit(
                    BudgetScope::Owner(record.id),
                    BudgetDimension::Bytes,
                    base.bytes,
                    actual.bytes,
                    record.limits.owner_bytes,
                )?,
                items: base.items.checked_add(actual.items).ok_or(BudgetError::Overflow)?,
            };
            let class_amount = ResourceAmount {
                bytes: Self::validate_limit(
                    BudgetScope::OwnerClass { owner: record.id, class },
                    BudgetDimension::Bytes,
                    class_base.bytes,
                    actual.bytes,
                    record.limits.class_bytes[class],
                )?,
                items: Self::validate_limit(
                    BudgetScope::OwnerClass { owner: record.id, class },
                    BudgetDimension::Items,
                    class_base.items,
                    actual.items,
                    record.limits.class_items[class].unwrap_or(usize::MAX),
                )?,
            };
            owner_next.push((amount, class_amount));
        }
        if actual.bytes > current.bytes {
            // When: `actual.bytes` grows, process admission is the last fallible step before any shard changes.
            let growth = actual.bytes - current.bytes;
            let mut process = self.process_bytes.load(Ordering::Acquire);
            loop {
                if process < current.bytes {
                    // When: `process` cannot cover this token, refuse before publishing a corrupt process total.
                    return Err(invariant());
                }
                let candidate = Self::validate_limit(
                    BudgetScope::Process,
                    BudgetDimension::Bytes,
                    process,
                    growth,
                    self.limits.process_bytes,
                )?;
                match self.process_bytes.compare_exchange_weak(
                    process,
                    candidate,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        // When: compare_exchange_weak returns Ok, growth is admitted and only infallible shard writes remain.
                        break;
                    }
                    Err(observed) => process = observed,
                }
            }
        } else if self.process_bytes.load(Ordering::Acquire) < current.bytes {
            // When: process accounting cannot cover `current.bytes`, retain every old balance rather than underflow.
            return Err(invariant());
        }
        class_usage.bytes = class_next.bytes;
        class_usage.items = class_next.items;
        class_usage.epoch = class_usage.epoch.wrapping_add(1);
        for (usage, (amount, class_amount)) in owner_usage.iter_mut().zip(owner_next) {
            usage.amount = amount;
            usage.class_bytes[class] = class_amount.bytes;
            usage.class_items[class] = class_amount.items;
            usage.epoch = usage.epoch.wrapping_add(1);
        }
        if actual.bytes < current.bytes {
            self.process_bytes.fetch_sub(current.bytes - actual.bytes, Ordering::AcqRel);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "ledger_resize_tests.rs"]
mod ledger_resize_tests;
