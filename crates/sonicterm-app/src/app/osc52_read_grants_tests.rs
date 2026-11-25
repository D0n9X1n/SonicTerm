use super::*;
use std::time::Instant;

/// A binding for pane `pane_id` with every epoch at zero.
fn binding(pane_id: u64) -> ReadBinding {
    ReadBinding {
        pane_id,
        incarnation: 0,
        owner_epoch: 0,
        reset_epoch: 0,
        command_epoch: 0,
        policy_generation: 0,
        created_at: Instant::now(),
    }
}

fn credential(name: &str, password: &str) -> GrantCredential {
    GrantCredential::new(name, password).expect("a usable credential")
}

/// Contract: a credential needs a nonempty name and a nonempty password; its `Debug` never shows
/// either, and its display name is cut to 64 characters with control characters replaced.
#[test]
fn credential_requires_name_and_hides_password() {
    assert!(GrantCredential::new("", "pw").is_none(), "no name: no lookup and no grant");
    assert!(GrantCredential::new("tool", "").is_none(), "no password: no lookup and no grant");
    let named = credential("to\u{1b}ol", "hunter2");
    assert_eq!(named.display_name(), "to\u{FFFD}ol");
    let printed = format!("{named:?}");
    assert!(!printed.contains("hunter2") && !printed.contains("ol"), "{printed}");
    assert_eq!(credential(&"x".repeat(100), "pw").display_name().chars().count(), 64);
}

/// Contract: a grant matches only the pane, PTY incarnation, owner and reset epochs, operation,
/// location and password it was recorded under; the program-supplied name is never part of the key.
#[test]
fn grant_key_binds_every_scope_field() {
    let standard = KittyClipboardLocation::Standard;
    let key = |binding: &ReadBinding, write, location, name: &str, password: &str| {
        GrantKey::new(binding, write, location, &credential(name, password))
    };
    let mut grants = SessionGrants::default();
    grants.record(key(&binding(1), false, standard, "tool", "pw"), GrantDecision::Always);
    assert_eq!(
        grants.lookup(&key(&binding(1), false, standard, "renamed", "pw")),
        Some(GrantDecision::Always),
        "the name is display-only"
    );
    let mut moved = Vec::new();
    for change in 0..4 {
        let mut other = binding(1);
        match change {
            0 => other.pane_id = 2,
            1 => other.incarnation = 1,
            2 => other.owner_epoch = 1,
            _ => other.reset_epoch = 1,
        }
        moved.push(key(&other, false, standard, "tool", "pw"));
    }
    moved.push(key(&binding(1), true, standard, "tool", "pw"));
    moved.push(key(&binding(1), false, KittyClipboardLocation::Primary, "tool", "pw"));
    moved.push(key(&binding(1), false, standard, "tool", "other"));
    for (index, other) in moved.iter().enumerate() {
        assert_eq!(grants.lookup(other), None, "scope change {index}");
    }
}

/// Contract: a pane keeps at most 16 grants and evicts its oldest; another pane's grants are kept,
/// re-recording a key replaces its decision, and `clear` drops every grant.
#[test]
fn sixteen_grants_per_pane_oldest_evicted() {
    let standard = KittyClipboardLocation::Standard;
    let key = |pane_id, password: &str| {
        GrantKey::new(&binding(pane_id), false, standard, &credential("tool", password))
    };
    let mut grants = SessionGrants::default();
    grants.record(key(2, "kept"), GrantDecision::Ban);
    for index in 0..=MAX_GRANTS_PER_PANE {
        grants.record(key(1, &index.to_string()), GrantDecision::Always);
    }
    assert_eq!(grants.lookup(&key(1, "0")), None, "the oldest grant of pane 1 was evicted");
    assert_eq!(grants.lookup(&key(1, "16")), Some(GrantDecision::Always));
    assert_eq!(grants.lookup(&key(2, "kept")), Some(GrantDecision::Ban));
    assert_eq!(grants.len(), MAX_GRANTS_PER_PANE + 1);
    grants.record(key(2, "kept"), GrantDecision::Always);
    assert_eq!(grants.lookup(&key(2, "kept")), Some(GrantDecision::Always));
    assert_eq!(grants.len(), MAX_GRANTS_PER_PANE + 1, "a replaced key is not duplicated");
    grants.retain(|key| key.pane_id != 1);
    assert_eq!(grants.len(), 1);
    grants.clear();
    assert!(grants.is_empty());
}

/// A paste binding for pane `pane_id` with every epoch at zero.
fn paste_binding(pane_id: u64) -> PasteBinding {
    PasteBinding {
        pane_id,
        incarnation: 0,
        owner_epoch: 0,
        reset_epoch: 0,
        paste_mode: 1,
        policy_generation: 0,
    }
}

fn paste_grant(pane_id: u64, password: &str) -> PasteGrant {
    let standard = KittyClipboardLocation::Standard;
    PasteGrant::new(paste_binding(pane_id), standard, 7, password.into(), Instant::now())
}

/// Contract: a one-time password is fresh from the OS CSPRNG each time, carries 16 random bytes as
/// Base64 text (so the `pw` field stays UTF-8), and a grant's `Debug` never prints it.
#[test]
fn one_time_password_is_fresh_and_hidden() {
    use base64::Engine as _;
    let first = one_time_password().expect("the OS CSPRNG answers");
    let second = one_time_password().expect("the OS CSPRNG answers");
    assert_ne!(first, second, "every paste gets its own password");
    let decoded = base64::engine::general_purpose::STANDARD.decode(&first).expect("Base64 text");
    assert_eq!(decoded.len(), 16);
    let printed = format!("{:?}", paste_grant(1, &first));
    assert!(!printed.contains(&first), "{printed}");
}

/// Contract: a pane holds one paste grant; a new one replaces it, a take matches the pane, the
/// location and the password exactly and removes the grant, so a second take of the same
/// password finds nothing; revoking drops only that pane's grant.
#[test]
fn paste_grants_one_per_pane_taken_once() {
    let mut grants = PasteGrants::default();
    let standard = KittyClipboardLocation::Standard;
    grants.issue(paste_grant(1, "old"));
    grants.issue(paste_grant(1, "new"));
    grants.issue(paste_grant(2, "other"));
    assert_eq!(grants.len(), 2, "a new paste replaced pane 1's grant");
    assert!(grants.take(1, standard, "old").is_none(), "the replaced password is dead");
    assert!(grants.take(2, standard, "new").is_none(), "another pane's grant never matches");
    assert!(
        grants.take(1, KittyClipboardLocation::Primary, "new").is_none(),
        "nor another location"
    );
    assert_eq!(grants.take(1, standard, "new").map(|grant| grant.offer_version), Some(7));
    assert!(grants.take(1, standard, "new").is_none(), "consumed once");
    grants.revoke(2);
    assert!(grants.is_empty());
}
