use sonicterm_fontconfig::{FcChar8, FcResultMatch, FcTypeString};

#[test]
fn exports_raw_fontconfig_abi_types() {
    assert_eq!(std::mem::size_of::<FcChar8>(), 1);
    assert_eq!(FcResultMatch, 0);
    assert_eq!(FcTypeString, 3);
}
