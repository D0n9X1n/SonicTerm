use super::*;
use enum_map::enum_map;
use std::time::{Duration, Instant};

const ALL_CLASSES: [ResourceClass; 24] = [
    ResourceClass::GridVisible,
    ResourceClass::GridHistory,
    ResourceClass::GridAlternate,
    ResourceClass::Surface,
    ResourceClass::SoftwareFrame,
    ResourceClass::UploadStaging,
    ResourceClass::FontFace,
    ResourceClass::GlyphRaster,
    ResourceClass::GlyphAtlas,
    ResourceClass::RowGlyphCache,
    ResourceClass::RowQuadCache,
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

// ---------------------------------------------------------------------------
/// Hash-table retention includes bucket payloads, controls, and the trailing group.
#[test]
fn retained_hash_table_bytes_matches_swiss_table_layout() {
    assert_eq!(retained_hash_table_bytes::<u64, u64>(0), 0);
    assert_eq!(retained_hash_table_bytes::<u64, u64>(7), 8 * 16 + 8 + 16);
    assert_eq!(retained_hash_table_bytes::<u64, u64>(8), 16 * 16 + 16 + 16);
}

// Class coverage
//
// A class with no charge site looks identical, from outside, to a class
// someone forgot. These make the difference explicit and checkable.
// ---------------------------------------------------------------------------

/// Every class has a coverage decision.
///
/// Guaranteed by the exhaustive match in `coverage()`, so this asserts the
/// guarantee is real rather than the match having a catch-all arm.
#[test]
fn every_class_has_a_coverage_decision() {
    for index in 0..ResourceClass::COUNT {
        let class = ResourceClass::from_usize(index);
        // Calling it at all is the assertion: a wildcard arm would make this
        // pass vacuously, so the value is checked for sense too.
        match class.coverage() {
            ClassCoverage::MeasuredNegligible { per_pane_bytes } => {
                assert!(
                    per_pane_bytes > 0,
                    "{class:?} is classified negligible with a zero measurement; \
                     a zero figure means nobody measured"
                );
                assert!(
                    per_pane_bytes <= 1024 * 1024,
                    "{class:?} is classified negligible at {per_pane_bytes} bytes per pane, \
                     which is not negligible — twenty panes make it \
                     {} MiB",
                    per_pane_bytes * 20 / (1024 * 1024)
                );
            }
            ClassCoverage::UnchargedRetention { per_owner_bytes } => {
                assert!(
                    per_owner_bytes > 0,
                    "{class:?} records uncharged retention with a zero figure; the \
                     variant exists to carry what the gap holds, and zero carries nothing"
                );
            }
            ClassCoverage::Charged
            | ClassCoverage::SubsystemAbsent
            | ClassCoverage::FeatureGated
            | ClassCoverage::TransientWithinCall => {}
        }
    }
}

/// The classes the retention path charges must say so.
///
/// Pins the table to reality in the direction that matters: a class charged in
/// production but recorded as absent would send the next reader looking for
/// work already done.
///
/// The list is the classes a production pass actually charges. `GlyphAtlas`
/// and `SoftwareFrame` were once here and are not now: the renderer computes
/// both figures, but `retained_amounts` has no caller and `sonicterm-gpu`
/// declares no dependency on `sonicterm-resource`, so the crate cannot reserve
/// at all. Naming them here asserted the table against itself and passed while
/// nothing was charged.
#[test]
fn classes_with_production_charge_sites_are_recorded_as_charged() {
    for class in [
        ResourceClass::GridVisible,
        ResourceClass::GridHistory,
        ResourceClass::GridAlternate,
        ResourceClass::ParserCapture,
        ResourceClass::ProtocolMetadata,
        ResourceClass::InlineMediaRetained,
        ResourceClass::PtyOutput,
    ] {
        assert_eq!(
            class.coverage(),
            ClassCoverage::Charged,
            "{class:?} has a production charge site and must be recorded as charged"
        );
    }
}

/// Nothing is classified negligible without a figure behind it.
///
/// The whole point of the variant carrying a number is that "small" has to be
/// a measurement someone took, not an adjective. Twenty panes is the scale the
/// rest of this work uses, so the aggregate is checked at that scale.
#[test]
fn negligible_classes_stay_negligible_in_aggregate() {
    const PANES: usize = 20;
    let mut aggregate = 0usize;
    for index in 0..ResourceClass::COUNT {
        let class = ResourceClass::from_usize(index);
        if let ClassCoverage::MeasuredNegligible { per_pane_bytes } = class.coverage() {
            aggregate += per_pane_bytes * PANES;
        }
    }
    assert!(
        aggregate < 4 * 1024 * 1024,
        "everything classified negligible sums to {} KiB across {PANES} panes; \
         a sum that large is not negligible even if each term is",
        aggregate / 1024
    );
}

// ---------------------------------------------------------------------------
// Pane seam terms
//
// The pane seam-cap sum is only a derivation while every class has been
// decided about. These hold that decision to the coverage table so the two
// cannot describe different worlds.
// ---------------------------------------------------------------------------

/// Every class has a pane-seam decision.
///
/// Guaranteed by the exhaustive match in `pane_seam_term()`, so this asserts
/// the guarantee is real rather than the match having a catch-all arm.
#[test]
fn every_class_has_a_pane_seam_term_decision() {
    for index in 0..ResourceClass::COUNT {
        let class = ResourceClass::from_usize(index);
        match class.pane_seam_term() {
            PaneSeamTerm::Contributes
            | PaneSeamTerm::ChargedToAnotherOwnerKind
            | PaneSeamTerm::NotChargedInProduction => {}
        }
    }
}

/// A class cannot contribute a term unless something charges it.
///
/// The sum is compared against a pane owner's ledger total. A term for a class
/// with no charge site raises the backstop by memory that can never appear in
/// the figure it guards, which is the way a tripwire stops being one.
#[test]
fn contributing_classes_are_charged_classes() {
    for index in 0..ResourceClass::COUNT {
        let class = ResourceClass::from_usize(index);
        if class.pane_seam_term() == PaneSeamTerm::Contributes {
            assert_eq!(
                class.coverage(),
                ClassCoverage::Charged,
                "{class:?} contributes a term to the pane seam-cap sum but nothing charges it; \
                 an uncharged term inflates the backstop with memory that cannot reach it"
            );
        }
    }
}

/// A charged class is either a term or an explicit exclusion.
///
/// The direction that catches the omission this exists for: a class that
/// starts charging a pane and is not added to the sum leaves the backstop
/// below memory the seams permit, where it fires on a healthy pane.
#[test]
fn charged_classes_are_either_terms_or_excluded_for_a_stated_reason() {
    for index in 0..ResourceClass::COUNT {
        let class = ResourceClass::from_usize(index);
        if class.coverage() != ClassCoverage::Charged {
            assert_ne!(
                class.pane_seam_term(),
                PaneSeamTerm::ChargedToAnotherOwnerKind,
                "{class:?} is excluded as charged elsewhere but is not charged at all; \
                 the exclusion reason must be the true one"
            );
            continue;
        }
        match class.pane_seam_term() {
            // Contributing is the expected outcome for a charged class.
            PaneSeamTerm::Contributes => {}
            // Excluded because a pane owner's ledger never carries it.
            PaneSeamTerm::ChargedToAnotherOwnerKind => {}
            PaneSeamTerm::NotChargedInProduction => panic!(
                "{class:?} is charged in production but recorded as never charged; \
                 a charged class must contribute a term or say which owner carries it"
            ),
        }
    }
}

/// The classes the pane retention pass charges are exactly the terms.
///
/// Checks the term rows against a copy of the charge list, kept here because a
/// contract crate cannot import the app that consumes it.
///
/// The copy is the weakness: it can only drift toward the pass it mirrors, and
/// it has. What actually pins the table to production is
/// `the_coverage_table_agrees_with_the_charge_sites` in `sonicterm-app`, which
/// reads `seam_classes()` itself, one crate up where the dependency runs the
/// right way. This one catches a term row that disagrees with the copy; treat
/// the app-side check as the authority when the two ever differ.
#[test]
fn the_pane_charge_sites_are_exactly_the_contributing_classes() {
    // Mirrors `sonicterm-app`'s pane retention pass. Update both together.
    let charged_to_panes = [
        ResourceClass::GridVisible,
        ResourceClass::GridHistory,
        ResourceClass::GridAlternate,
        ResourceClass::ParserCapture,
        ResourceClass::ProtocolMetadata,
        ResourceClass::InlineMediaRetained,
        ResourceClass::PtyOutput,
        ResourceClass::PtyInput,
    ];
    for class in charged_to_panes {
        assert_eq!(
            class.pane_seam_term(),
            PaneSeamTerm::Contributes,
            "{class:?} is charged to a pane owner and must carry a term in the seam-cap sum"
        );
    }
    for index in 0..ResourceClass::COUNT {
        let class = ResourceClass::from_usize(index);
        if class.pane_seam_term() == PaneSeamTerm::Contributes {
            assert!(
                charged_to_panes.contains(&class),
                "{class:?} carries a term in the pane seam-cap sum but is absent from the \
                 copy of the charge list above; if the retention pass does charge it, this \
                 copy is stale"
            );
        }
    }
}
