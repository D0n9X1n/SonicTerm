use enum_map::EnumMap;
use parking_lot::{Mutex, RwLock};
use sonicterm_types::{
    BudgetError, OwnerKind, OwnerLimits, OwnerState, ResourceAmount, ResourceClass, ResourceOwnerId,
};
use std::{collections::HashMap, sync::Arc};

const REGISTRY_SHARDS: usize = 16;

pub(crate) struct OwnerRecord {
    pub(crate) id: ResourceOwnerId,
    pub(crate) kind: OwnerKind,
    pub(crate) parent: Option<Arc<OwnerRecord>>,
    pub(crate) limits: OwnerLimits,
    pub(crate) state: RwLock<OwnerState>,
    pub(crate) usage: Mutex<OwnerUsage>,
}

impl OwnerRecord {
    pub(crate) fn path(owner: &Arc<Self>) -> Vec<Arc<Self>> {
        let mut path = Vec::new();
        let mut current = Some(owner.clone());
        while let Some(record) = current {
            current = record.parent.clone();
            path.push(record);
        }
        path.reverse();
        path
    }
}

#[derive(Clone)]
pub(crate) struct OwnerUsage {
    pub(crate) amount: ResourceAmount,
    pub(crate) class_bytes: EnumMap<ResourceClass, usize>,
    pub(crate) class_items: EnumMap<ResourceClass, usize>,
    pub(crate) open_children: usize,
    pub(crate) epoch: u64,
}

impl OwnerUsage {
    pub(crate) fn open() -> Self {
        Self {
            amount: ResourceAmount::default(),
            class_bytes: EnumMap::default(),
            class_items: EnumMap::default(),
            open_children: 0,
            epoch: 0,
        }
    }
}

pub(crate) struct OwnerRegistry {
    shards: [RwLock<HashMap<ResourceOwnerId, Arc<OwnerRecord>>>; REGISTRY_SHARDS],
}

impl OwnerRegistry {
    pub(crate) fn new() -> Self {
        Self { shards: std::array::from_fn(|_| RwLock::new(HashMap::new())) }
    }

    fn shard(id: ResourceOwnerId) -> usize {
        id.get() as usize % REGISTRY_SHARDS
    }

    pub(crate) fn get(&self, id: ResourceOwnerId) -> Result<Arc<OwnerRecord>, BudgetError> {
        self.shards[Self::shard(id)].read().get(&id).cloned().ok_or(BudgetError::OwnerNotFound(id))
    }

    pub(crate) fn insert(&self, record: Arc<OwnerRecord>) {
        let previous = self.shards[Self::shard(record.id)].write().insert(record.id, record);
        debug_assert!(previous.is_none());
    }

    /// Drop a closed owner's record.
    ///
    /// Marking an owner `Closed` satisfies the lifecycle contract but returns
    /// no memory: an `OwnerRecord` carries two `EnumMap`s over every resource
    /// class, an `RwLock`, a `Mutex`, and an `Arc` to its parent, and a record
    /// left in its shard holds all of it for the life of the process. Measured
    /// at roughly 1 KiB per owner, growing linearly with every tab or pane
    /// opened and closed, while the governor reported zero bytes in use.
    ///
    /// A child holds an `Arc` to its parent, so a parent's record is freed
    /// only once its children are gone. Close order already guarantees that:
    /// `finish_close` refuses an owner with live children, so children are
    /// removed first and the last child's removal releases the parent.
    pub(crate) fn remove(&self, id: ResourceOwnerId) -> Option<Arc<OwnerRecord>> {
        self.shards[Self::shard(id)].write().remove(&id)
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod owner_tests;
