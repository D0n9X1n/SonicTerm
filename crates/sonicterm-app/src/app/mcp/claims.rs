//! Resume tokens and `claim_panes`: how an agent that reconnected takes back the panes its
//! earlier session created.
//!
//! Each session holds one canonical resume token, minted when it connects and readable through
//! `session_token`. The token is a bearer secret kept only in server memory: it is never logged
//! and never put in an error. A claim names a closed session's token and proves the caller holds
//! its own; on the event loop, in one step, the closed session's panes move to the caller and its
//! token is deleted, never kept as an alias. A revoking reload (`control`, `read`, `isolation`,
//! `create_anchor`, or a stop) drops every closed session's token and gives each live session a
//! fresh one. A closed session's record is pruned once it owns no pane.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_cfg::config::McpControl;

use super::agents::{Access, AgentSessionId, PaneOwner};
use super::LoopReply;
use crate::app::App;

/// A session's 128-bit resume token. Its `Debug` hides the value, so no log prints it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResumeToken([u8; 16]);

impl std::fmt::Debug for ResumeToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResumeToken(..)")
    }
}

impl ResumeToken {
    /// A fresh token from the OS random source; `None` when it fails.
    fn mint() -> Option<Self> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).ok().map(|()| Self(bytes))
    }

    /// The token as 32 lowercase hex digits, the form an agent holds.
    pub(crate) fn to_hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// The token `text` spells as 32 hex digits; `None` for anything else.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        if text.len() != 32 || !text.is_ascii() {
            // When: `text` is not 32 ASCII characters, it cannot be a token.
            return None;
        }
        let mut bytes = [0u8; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(Self(bytes))
    }

    /// Compare without an early exit, so timing does not reveal how much of a guess matched.
    fn matches(&self, other: &Self) -> bool {
        self.0.iter().zip(other.0).fold(0u8, |diff, (left, right)| diff | (left ^ right)) == 0
    }
}

/// What the registry knows of one session.
struct SessionRecord {
    /// The connection is still open.
    live: bool,
    /// The connection's call slot, set from a call's admission until its result is formed; a
    /// closed session with a call still running is not drained. `None` for a session with no slot.
    in_flight: Option<Arc<AtomicBool>>,
    /// The session's one canonical token; `None` when minting failed.
    token: Option<ResumeToken>,
}

/// Why a claim was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimRefusal {
    /// No closed or live session holds that token, or the caller's own token is wrong: the two
    /// read the same, so a probe learns nothing.
    NotFound,
    /// The token's session is still connected, or a call it admitted is still running.
    SessionLive,
}

/// Every session's record, behind one lock held only for a lookup or an update.
#[derive(Default)]
pub(crate) struct ResumeRegistry {
    /// Live sessions, and closed sessions that may still own panes.
    records: Mutex<HashMap<AgentSessionId, SessionRecord>>,
    /// Makes minting fail, so a test can see a session run without a token.
    #[cfg(test)]
    pub(crate) fail_mint: AtomicBool,
}

impl ResumeRegistry {
    /// A token, or `None` when the OS random source (or a test) refuses.
    // Ordering: fail_mint Relaxed; a test sets it before any connection it affects opens.
    fn mint(&self) -> Option<ResumeToken> {
        #[cfg(test)]
        if self.fail_mint.load(Ordering::Relaxed) {
            // When: a test set `fail_mint`, minting fails as a broken random source would.
            return None;
        }
        let token = ResumeToken::mint();
        if token.is_none() {
            // The OS random source failed, so the session runs without a token.
            tracing::warn!(target: "mcp", "no resume token: the OS random source failed");
        }
        token
    }

    /// Record `session` as connected, with its call slot, and mint its token.
    pub(crate) fn connect(&self, session: AgentSessionId, in_flight: Option<Arc<AtomicBool>>) {
        let token = self.mint();
        self.records.lock().insert(session, SessionRecord { live: true, in_flight, token });
    }

    /// Record `session` as closed. Without a token nothing can claim it, so it is forgotten now.
    pub(crate) fn disconnect(&self, session: AgentSessionId) {
        let mut records = self.records.lock();
        if let Some(record) = records.get_mut(&session) {
            record.live = false;
            if record.token.is_none() {
                // Without a token nothing could ever name the session.
                records.remove(&session);
            }
        }
    }

    /// `session`'s current token.
    pub(crate) fn token_of(&self, session: AgentSessionId) -> Option<ResumeToken> {
        self.records.lock().get(&session).and_then(|record| record.token)
    }

    /// After a revoking reload: drop every closed session's token (and so its record), and give
    /// each live session a fresh one, which `session_token` returns from now on.
    pub(crate) fn reissue(&self) {
        let mut records = self.records.lock();
        records.retain(|_, record| record.live);
        for record in records.values_mut() {
            record.token = self.mint();
        }
    }

    /// Forget every closed session that owns none of `owners`.
    pub(crate) fn prune(&self, owners: &HashSet<AgentSessionId>) {
        self.records.lock().retain(|session, record| record.live || owners.contains(session));
    }

    /// How many records are kept, so a test can bound the registry.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.records.lock().len()
    }

    /// Check a claim by `caller` holding `own` for the session holding `resume`, and on success
    /// delete that session's record in the same step. Returns the session whose panes move.
    // Ordering: slot Acquire load pairs with run_calls' in_flight Release store after a result.
    fn take(
        &self,
        caller: AgentSessionId,
        resume: &ResumeToken,
        own: &ResumeToken,
    ) -> Result<AgentSessionId, ClaimRefusal> {
        let mut records = self.records.lock();
        let owns = records.get(&caller).and_then(|record| record.token);
        if !owns.is_some_and(|token| token.matches(own)) {
            // When: the caller's `own` token is not its current one, nothing moves.
            return Err(ClaimRefusal::NotFound);
        }
        let source = records
            .iter()
            .find(|(session, record)| {
                **session != caller && record.token.is_some_and(|token| token.matches(resume))
            })
            .map(|(session, record)| {
                let running =
                    record.in_flight.as_ref().is_some_and(|slot| slot.load(Ordering::Acquire));
                (*session, record.live || running)
            });
        let Some((source, live)) = source else {
            // When: `source` is None, no other session holds `resume`: wrong or already claimed.
            return Err(ClaimRefusal::NotFound);
        };
        if live {
            // When: the source is `live` or a call of its is still running, it is not drained.
            return Err(ClaimRefusal::SessionLive);
        }
        records.remove(&source);
        Ok(source)
    }
}

impl App {
    /// `claim_panes` on the loop: with control on, move every pane the session holding `resume`
    /// created to the caller, whose `own` token proves it received that token, and delete the
    /// source's token in the same step. READONLY does not refuse it: ownership moves, no pane is
    /// written, and READONLY still refuses every later write.
    pub(crate) fn mcp_claim_panes(
        &mut self,
        registry: &ResumeRegistry,
        access: Access,
        resume: &ResumeToken,
        own: &ResumeToken,
    ) -> LoopReply {
        if access.control == McpControl::Off {
            // When: `control` is `Off`, ownership never moves.
            return super::control_loop::Refusal::ControlOff.reply();
        }
        let source = match registry.take(access.session, resume, own) {
            Ok(source) => source,
            Err(ClaimRefusal::NotFound) => {
                // When: `take` found no `NotFound` match, the reply reveals nothing more.
                return LoopReply::Refused(
                    "not_found",
                    "no closed session holds that token".into(),
                );
            }
            Err(ClaimRefusal::SessionLive) => {
                // When: `take` says `SessionLive`, the claim is refused at once, never waited on.
                return LoopReply::Refused(
                    "session_live",
                    "that session is still connected or finishing a call".into(),
                );
            }
        };
        let mut claimed = 0;
        for window in self.registry.windows.values_mut() {
            for pane in window.panes.values_mut() {
                if pane.creator == PaneOwner::Agent(source) {
                    pane.creator = PaneOwner::Agent(access.session);
                    claimed += 1;
                }
            }
        }
        self.prune_resume_tokens(registry);
        tracing::info!(target: "mcp", panes = claimed, "agent claimed panes");
        LoopReply::Claimed(claimed)
    }
}

impl App {
    /// Forget every closed session that no longer owns a pane, so the registry holds only live
    /// sessions and closed ones a claim could still move panes from. Runs after each claim and on
    /// every retention pass, so a session's record goes at most one interval after its last pane.
    pub(crate) fn prune_resume_tokens(&self, registry: &ResumeRegistry) {
        let owners: HashSet<AgentSessionId> = self
            .registry
            .windows
            .values()
            .flat_map(|window| window.panes.values())
            .filter_map(|pane| match pane.creator {
                PaneOwner::Agent(session) => Some(session),
                PaneOwner::User => None,
            })
            .collect();
        registry.prune(&owners);
    }
}

#[cfg(test)]
#[path = "claims_tests.rs"]
mod claims_tests;
