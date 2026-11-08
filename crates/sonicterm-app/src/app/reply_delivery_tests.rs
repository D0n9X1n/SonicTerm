//! Pins the VT worker's reply policy: a full disk budget drops one reply and keeps sending.

use std::io;

use super::{ReplyDelivery, ReplyVerdict};

/// The error a spool returns when its disk budget refuses a reply.
fn budget() -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::QuotaExceeded, sonicterm_io::ReplyBudgetExceeded))
}

/// A budget refusal warns once per backlog and never stops later replies; admissions while the
/// backlog is still above half the budget do not re-arm the warning, one below half does.
#[test]
fn budget_refusals_warn_once_per_backlog_and_keep_sending() {
    let mut delivery = ReplyDelivery::default();
    let full = || sonicterm_io::SPILL_BUDGET_BYTES;
    let drained = || sonicterm_io::SPILL_BUDGET_BYTES / 2 - 1;
    assert_eq!(delivery.record(&budget(), full), ReplyVerdict::BudgetWarn);
    assert_eq!(delivery.record(&budget(), full), ReplyVerdict::Quiet);
    assert!(delivery.accepting(), "a budget refusal must not stop later replies");
    // A slow reader frees one record: the reply is admitted, but the backlog is still nearly full.
    assert_eq!(delivery.record(&Ok(()), full), ReplyVerdict::Quiet);
    assert_eq!(delivery.record(&budget(), full), ReplyVerdict::Quiet, "re-armed too early");
    // The backlog drained below half: the next refusal is a new backlog.
    assert_eq!(delivery.record(&Ok(()), drained), ReplyVerdict::Quiet);
    assert_eq!(delivery.record(&budget(), full), ReplyVerdict::BudgetWarn);
}

/// Any other error latches: it is reported once and every later reply is dropped.
#[test]
fn storage_failure_latches() {
    let mut delivery = ReplyDelivery::default();
    let failure = Err(io::Error::from(io::ErrorKind::StorageFull));
    assert_eq!(delivery.record(&failure, || 0), ReplyVerdict::Failed);
    assert!(!delivery.accepting());
}

/// A native filesystem quota failure shares the error kind but latched the spool: it stops
/// delivery like any storage failure instead of being treated as backlog pressure.
#[test]
fn a_native_quota_failure_is_a_failure_not_a_budget_drop() {
    let mut delivery = ReplyDelivery::default();
    let native = Err(io::Error::from(io::ErrorKind::QuotaExceeded));
    assert_eq!(delivery.record(&native, || 0), ReplyVerdict::Failed);
    assert!(!delivery.accepting());
}
