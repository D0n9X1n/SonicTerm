//! Deterministic helpers for contract and downstream tests.

use crate::ResourceGovernor;
use enum_map::enum_map;
use sonicterm_types::{GovernorLimits, ProcessKind};

/// Create a real governor with effectively unlimited immutable limits.
///
/// Accounting, owner state, and exact-once RAII behavior remain enabled.
pub fn unlimited_governor(kind: ProcessKind) -> ResourceGovernor {
    ResourceGovernor::new(
        kind,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .expect("unlimited test governor")
}

#[cfg(test)]
#[path = "test_support_tests.rs"]
mod test_support_tests;
