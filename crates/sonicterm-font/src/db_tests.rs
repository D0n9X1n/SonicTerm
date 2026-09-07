use super::FontDatabase;

/// The built-in slot stays empty because St.Helens uses assets and other fallbacks use native discovery.
#[test]
fn built_in_database_contains_no_private_vendor_bundles() {
    let database = FontDatabase::with_built_in().expect("empty database construction succeeds");
    assert!(database.list_available().is_empty());
}
