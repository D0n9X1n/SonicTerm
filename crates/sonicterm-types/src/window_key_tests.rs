//! The `WindowKey` contract: a lossless `u64` wrapper with a stable display form.

use super::WindowKey;

#[test]
fn every_constructor_keeps_the_raw_id() {
    // Contract: `new`, `From<u64>` and the tuple field agree, and `raw` returns the id unchanged.
    let key_id = u64::MAX - 7;
    assert_eq!(WindowKey::new(key_id), WindowKey::from(key_id));
    assert_eq!(WindowKey::new(key_id), WindowKey(key_id));
    assert_eq!(WindowKey::new(key_id).raw(), key_id);
}

#[test]
fn display_names_the_type_and_id() {
    // Contract: log lines and diagnostics print a key as `WindowKey(<id>)`.
    assert_eq!(WindowKey::new(42).to_string(), "WindowKey(42)");
}
