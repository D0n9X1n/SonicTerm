use super::*;
use enum_map::enum_map;
use std::time::{Duration, Instant};

const ALL_CLASSES: [ResourceClass; 22] = [
    ResourceClass::GridVisible,
    ResourceClass::GridHistory,
    ResourceClass::GridAlternate,
    ResourceClass::Surface,
    ResourceClass::SoftwareFrame,
    ResourceClass::UploadStaging,
    ResourceClass::FontFace,
    ResourceClass::GlyphRaster,
    ResourceClass::GlyphAtlas,
    ResourceClass::ParserCapture,
    ResourceClass::InlineMediaDecode,
    ResourceClass::InlineMediaRetained,
    ResourceClass::PtyOutput,
    ResourceClass::PtyInput,
    ResourceClass::ParserReply,
    ResourceClass::RemoteInput,
    ResourceClass::RemoteOutput,
    ResourceClass::ProtocolMetadata,
    ResourceClass::MuxSubscriber,
    ResourceClass::CommandEvents,
    ResourceClass::RegistryMetadata,
    ResourceClass::ReaperWork,
];

#[test]
fn resource_owner_id_is_nonzero_and_read_only() {
    assert!(ResourceOwnerId::new(0).is_none());
    let id = ResourceOwnerId::new(42).expect("nonzero owner id");
    assert_eq!(id.get(), 42);
}

#[test]
fn resource_class_contract_has_every_normative_variant() {
    let mut seen = enum_map! { _ => false };
    for class in ALL_CLASSES {
        assert!(!seen[class], "duplicate class {class:?}");
        seen[class] = true;
    }
    assert!(seen.values().all(|present| *present));
}

#[test]
fn resource_amount_checked_arithmetic_is_component_wise() {
    let left = ResourceAmount { bytes: 7, items: 3 };
    let right = ResourceAmount { bytes: 5, items: 2 };
    assert_eq!(left.checked_add(right), Ok(ResourceAmount { bytes: 12, items: 5 }));
    assert_eq!(left.checked_sub(right), Ok(ResourceAmount { bytes: 2, items: 1 }));
    assert!(right.component_le(left));
    assert!(!left.component_le(right));
    assert!(ResourceAmount::default().is_zero());
}

#[test]
fn resource_amount_reports_overflow_and_underflow_without_saturation() {
    let overflow = ResourceAmount { bytes: usize::MAX, items: 0 }
        .checked_add(ResourceAmount { bytes: 1, items: 0 });
    assert_eq!(overflow, Err(BudgetError::Overflow));

    let underflow =
        ResourceAmount { bytes: 1, items: 0 }.checked_sub(ResourceAmount { bytes: 0, items: 1 });
    assert_eq!(
        underflow,
        Err(BudgetError::AmountExceedsCharge {
            requested: ResourceAmount { bytes: 0, items: 1 },
            available: ResourceAmount { bytes: 1, items: 0 },
        })
    );
}

#[test]
fn limits_are_explicit_for_every_class() {
    let governor = GovernorLimits {
        process_bytes: 1_000,
        class_bytes: enum_map! { class => class as usize + 100 },
        class_items: enum_map! { class => Some(class as usize + 1) },
    };
    let owner = OwnerLimits {
        owner_bytes: 500,
        class_bytes: governor.class_bytes,
        class_items: governor.class_items,
    };

    for class in ALL_CLASSES {
        assert!(governor.class_bytes[class] >= 100);
        assert!(owner.class_items[class].is_some());
    }
}

#[test]
fn process_and_owner_kinds_cover_the_normative_hierarchy() {
    let process_kinds = [ProcessKind::Gui, ProcessKind::Mux];
    let owner_kinds = [
        OwnerKind::Process,
        OwnerKind::SharedFont,
        OwnerKind::SharedRaster,
        OwnerKind::SharedAtlas,
        OwnerKind::Window,
        OwnerKind::AppPane,
        OwnerKind::LocalPty,
        OwnerKind::MuxSession,
        OwnerKind::MuxPane,
        OwnerKind::PtyTransport,
        OwnerKind::MuxConnection,
        OwnerKind::Attachment,
    ];
    assert_eq!(process_kinds.len(), 2);
    assert_eq!(owner_kinds.len(), 12);
}

#[test]
fn retry_token_exposes_deadline_and_wakeup_contract() {
    let deadline = Instant::now() + Duration::from_millis(250);
    for wakeup in [
        RetryWakeup::AtDeadline,
        RetryWakeup::CapacityAvailable,
        RetryWakeup::ConnectionStateChanged,
    ] {
        let token = RetryToken::new(deadline, wakeup);
        assert_eq!(token.deadline(), deadline);
        assert_eq!(token.wakeup(), wakeup);
    }
}

#[test]
fn pressure_outcomes_preserve_caller_owned_values() {
    let deadline = Instant::now();
    let retry = RetryToken::new(deadline, RetryWakeup::CapacityAvailable);
    let outcome = PressureOutcome::Backpressured { value: vec![1, 2, 3], retry };
    match outcome {
        PressureOutcome::Backpressured { value, retry } => {
            assert_eq!(value, vec![1, 2, 3]);
            assert_eq!(retry.deadline(), deadline);
        }
        other => panic!("unexpected pressure outcome: {other:?}"),
    }

    let owner = ResourceOwnerId::new(7).unwrap();
    let rejected = PressureOutcome::Rejected {
        value: String::from("payload"),
        error: BudgetError::OwnerNotFound(owner),
    };
    assert!(matches!(
        rejected,
        PressureOutcome::Rejected { value, error: BudgetError::OwnerNotFound(id) }
            if value == "payload" && id == owner
    ));
}

#[test]
fn snapshot_exposes_owner_process_class_and_epoch_views() {
    let owner = ResourceOwnerId::new(9).unwrap();
    let snapshot = ResourceSnapshot {
        process_kind: ProcessKind::Gui,
        owner,
        owner_kind: OwnerKind::AppPane,
        owner_state: OwnerState::Open,
        parent: ResourceOwnerId::new(8),
        owner_amount: ResourceAmount { bytes: 12, items: 2 },
        owner_bytes_limit: 64,
        owner_class_bytes: enum_map! { _ => 0 },
        owner_class_items: enum_map! { _ => 0 },
        process_amount: ResourceAmount { bytes: 20, items: 3 },
        process_class_bytes: enum_map! { _ => 0 },
        process_class_items: enum_map! { _ => 0 },
        owner_epoch: 3,
        class_epochs: enum_map! { _ => 4 },
        registry_epoch: 5,
        release_failures: 0,
    };
    assert_eq!(snapshot.owner, owner);
    assert_eq!(
        snapshot.owner_bytes_limit, 64,
        "a snapshot must carry the limit its usage is measured against; usage without \
         a limit says how much is held and not whether that is near a problem"
    );
    assert_eq!(snapshot.parent.unwrap().get(), 8);
    assert_eq!(snapshot.owner_amount.bytes, 12);
    assert_eq!(snapshot.process_amount.items, 3);
    assert!(snapshot.class_epochs.values().all(|epoch| *epoch == 4));
    assert_eq!(snapshot.release_failures, 0);
}

#[test]
fn budget_errors_identify_scope_dimension_and_owner_state() {
    let owner = ResourceOwnerId::new(11).unwrap();
    let limit = BudgetError::LimitExceeded {
        scope: BudgetScope::OwnerClass { owner, class: ResourceClass::GlyphAtlas },
        dimension: BudgetDimension::Bytes,
        current: 100,
        requested: 50,
        limit: 128,
    };
    assert!(matches!(
        limit,
        BudgetError::LimitExceeded {
            scope: BudgetScope::OwnerClass { owner: id, class: ResourceClass::GlyphAtlas },
            dimension: BudgetDimension::Bytes,
            ..
        } if id == owner
    ));

    assert_eq!(
        BudgetError::InvalidOwnerState {
            owner,
            expected: OwnerState::Closing,
            actual: OwnerState::Open,
        },
        BudgetError::InvalidOwnerState {
            owner,
            expected: OwnerState::Closing,
            actual: OwnerState::Open,
        }
    );
}

#[test]
fn public_contract_types_are_send_sync_and_copy_where_expected() {
    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_copy<T: Copy>() {}

    assert_send_sync::<ResourceSnapshot>();
    assert_send_sync::<PressureOutcome<Vec<u8>>>();
    assert_copy::<ResourceOwnerId>();
    assert_copy::<ResourceClass>();
    assert_copy::<ResourceAmount>();
    assert_copy::<RetryToken>();
}
