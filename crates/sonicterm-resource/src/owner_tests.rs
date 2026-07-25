use super::*;
use enum_map::enum_map;

#[test]
fn owner_path_is_root_to_leaf_creation_order() {
    let limits = OwnerLimits {
        owner_bytes: 10,
        class_bytes: enum_map! { _ => 10 },
        class_items: enum_map! { _ => None },
    };
    let root = Arc::new(OwnerRecord {
        id: ResourceOwnerId::new(1).unwrap(),
        kind: OwnerKind::Process,
        parent: None,
        limits: limits.clone(),
        state: RwLock::new(OwnerState::Open),
        usage: Mutex::new(OwnerUsage::open()),
    });
    let leaf = Arc::new(OwnerRecord {
        id: ResourceOwnerId::new(2).unwrap(),
        kind: OwnerKind::AppPane,
        parent: Some(root),
        limits,
        state: RwLock::new(OwnerState::Open),
        usage: Mutex::new(OwnerUsage::open()),
    });
    assert_eq!(
        OwnerRecord::path(&leaf).iter().map(|owner| owner.id.get()).collect::<Vec<_>>(),
        [1, 2]
    );
}

#[test]
fn sharded_registry_returns_inserted_owner() {
    let registry = OwnerRegistry::new();
    let record = Arc::new(OwnerRecord {
        id: ResourceOwnerId::new(33).unwrap(),
        kind: OwnerKind::Window,
        parent: None,
        limits: OwnerLimits {
            owner_bytes: 0,
            class_bytes: EnumMap::default(),
            class_items: EnumMap::default(),
        },
        state: RwLock::new(OwnerState::Open),
        usage: Mutex::new(OwnerUsage::open()),
    });
    registry.insert(record.clone());
    assert!(Arc::ptr_eq(&registry.get(record.id).unwrap(), &record));
}
