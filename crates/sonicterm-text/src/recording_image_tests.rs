use super::*;
use crate::start_size_inputs::{InputSource, START_SIZE_INPUTS};

/// A CI-shaped host on `version` and `arch`.
fn host(version: Option<&str>, arch: Option<&str>, ci: bool) -> HostImage {
    HostImage {
        os: Some("macos14".into()),
        version: version.map(str::to_owned),
        arch: arch.map(str::to_owned),
        ci,
    }
}

/// Every platform with helper rows names the runner image they were recorded on.
#[test]
fn every_helper_platform_names_its_recording_image() {
    for row in START_SIZE_INPUTS.iter().filter(|row| row.source == InputSource::Helper) {
        assert!(
            RECORDING_IMAGES.iter().any(|image| image.platform == row.platform),
            "{} has helper rows but no recording image",
            row.platform
        );
    }
}

/// The recording image holds the rows exactly; another CI image fails, naming the image to
/// re-record on; a local host on another image, or with no image at all, gets advisories.
#[test]
fn the_host_image_decides_how_drift_is_treated() {
    let mac = RECORDING_IMAGES.iter().find(|image| image.platform == "macos").unwrap();
    let recorded = host(Some(mac.version), Some("arm64"), true);
    assert_eq!(drift_policy("macos", &recorded), DriftPolicy::Exact);
    let other = drift_policy("macos", &host(Some("20990101.1"), Some(mac.arch), true));
    assert!(
        matches!(&other, DriftPolicy::Rerecord(reason)
            if reason.starts_with("re-record the helper rows on macos14 20990101.1")),
        "{other:?}"
    );
    assert_eq!(drift_policy("macos", &host(None, None, false)), DriftPolicy::Advisory);
    let local = host(Some("20990101.1"), Some(mac.arch), false);
    assert_eq!(drift_policy("macos", &local), DriftPolicy::Advisory);
}
