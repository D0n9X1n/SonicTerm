//! One byte budget per `list_panes` reply for the optional per-pane text a program supplies, so
//! many panes with long values cannot push the reply past the bridge's line limit.
//!
//! Every such field is placed through [`ReplyBudget::place`]: it is written while its serialized
//! form fits what is left, and otherwise written as `null` with the field named under the pane's
//! `omitted` object and the reason, so an agent can tell an omitted value from an absent one.

use serde_json::Value;

/// The serialized bytes all budgeted fields of one reply may take together: a quarter of the
/// bridge's 1 MiB line limit, leaving room for every pane's fixed fields.
pub(crate) const REPLY_TEXT_BUDGET_BYTES: usize = 256 * 1024;

/// The serialized bytes one whole `list_panes` result may take, its fixed fields and every pane
/// record together: below the bridge's 1 MiB line limit, leaving room for the JSON-RPC envelope.
pub(crate) const REPLY_BYTES_LIMIT: usize = 768 * 1024;

/// The whole-result bound `list_panes` applies on this thread: [`REPLY_BYTES_LIMIT`], or the
/// smaller one a test set so a few panes can reach it.
pub(crate) fn reply_bytes_limit() -> usize {
    #[cfg(test)]
    if let Some(limit_bytes) = REPLY_LIMIT_OVERRIDE.get() {
        // When: a test set `REPLY_LIMIT_OVERRIDE` on this worker thread, its bound applies.
        return limit_bytes;
    }
    REPLY_BYTES_LIMIT
}

#[cfg(test)]
thread_local! {
    /// A test's whole-result bound for `list_panes` calls run on this thread.
    pub(crate) static REPLY_LIMIT_OVERRIDE: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// Why a field is `null` with a marker under `omitted`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Omission {
    /// The caller may not read the pane the value came from.
    Unreadable,
    /// The reply's budget was spent before this value.
    Budget,
}

impl Omission {
    /// The marker's wire name.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unreadable => "unreadable",
            Self::Budget => "budget",
        }
    }
}

/// What is left of one reply's budget.
pub(crate) struct ReplyBudget {
    /// Serialized bytes still available.
    remaining_bytes: usize,
}

impl ReplyBudget {
    /// A full budget of [`REPLY_TEXT_BUDGET_BYTES`] for one reply.
    pub(crate) fn new() -> Self {
        Self { remaining_bytes: REPLY_TEXT_BUDGET_BYTES }
    }

    /// Write `value` as `field` of `pane` when its serialized form fits what is left, charging it;
    /// a value that does not fit is `null` and marked [`Omission::Budget`]. `null` costs nothing.
    pub(crate) fn place(&mut self, pane: &mut Value, field: &str, value: Value) {
        if value.is_null() {
            // When: `value` is null there is no text to charge or omit.
            pane[field] = Value::Null;
            return;
        }
        // The serialized length counts the escapes a control or quote character adds on the wire.
        let cost_bytes = value.to_string().len();
        if cost_bytes > self.remaining_bytes {
            // When: `cost_bytes` exceeds `remaining_bytes`, the value is omitted, never cut.
            omit(pane, field, Omission::Budget);
            return;
        }
        self.remaining_bytes -= cost_bytes;
        pane[field] = value;
    }
}

/// Write `field` of `pane` as `null` and name it under the pane's `omitted` object with `reason`.
pub(crate) fn omit(pane: &mut Value, field: &str, reason: Omission) {
    pane[field] = Value::Null;
    pane["omitted"][field] = Value::from(reason.as_str());
}

#[cfg(test)]
#[path = "reply_budget_tests.rs"]
mod reply_budget_tests;
