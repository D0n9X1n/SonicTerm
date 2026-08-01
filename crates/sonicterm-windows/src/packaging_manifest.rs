pub(crate) const WINDOWS_WIX_MANIFEST: &str = include_str!("../wix/main.wxs");

#[cfg(test)]
#[path = "packaging_manifest_tests.rs"]
mod packaging_manifest_tests;
