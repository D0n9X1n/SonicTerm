use crate::ResourceGovernor;
use enum_map::enum_map;
use sonicterm_types::{GovernorLimits, OwnerKind, OwnerLimits, ProcessKind, ResourceOwnerId};

pub(super) fn limits(bytes: usize) -> GovernorLimits {
    GovernorLimits {
        process_bytes: bytes,
        class_bytes: enum_map! { _ => bytes },
        class_items: enum_map! { _ => Some(bytes) },
    }
}

pub(super) fn owner_limits(bytes: usize) -> OwnerLimits {
    OwnerLimits {
        owner_bytes: bytes,
        class_bytes: enum_map! { _ => bytes },
        class_items: enum_map! { _ => Some(bytes) },
    }
}

pub(super) fn governor(bytes: usize) -> ResourceGovernor {
    ResourceGovernor::new(ProcessKind::Gui, limits(bytes)).unwrap()
}

pub(super) fn app_pane(governor: &ResourceGovernor, bytes: usize) -> ResourceOwnerId {
    let window = governor
        .create_child(governor.root_owner(), OwnerKind::Window, owner_limits(bytes))
        .unwrap();
    governor.create_child(window, OwnerKind::AppPane, owner_limits(bytes)).unwrap()
}
