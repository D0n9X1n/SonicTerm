pub(crate) const MACOS_DMG_SCRIPT: &str = include_str!("../../../scripts/make-macos-dmg.sh");

#[cfg(test)]
#[path = "bundle_manifest_tests.rs"]
mod bundle_manifest_tests;
