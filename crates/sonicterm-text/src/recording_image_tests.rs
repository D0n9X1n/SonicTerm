use super::*;
use crate::start_size_inputs::{InputSource, START_SIZE_INPUTS};

/// A CI-shaped host on `version` and `arch`.
fn host(version: Option<&str>, arch: Option<&str>, on_ci: bool) -> HostImage {
    HostImage {
        os: Some("macos14".into()),
        version: version.map(str::to_owned),
        arch: arch.map(str::to_owned),
        on_ci,
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

/// The recording image holds the rows exactly, on CI or off. On CI any other image still compares
/// them exactly: identical rows pass with a note, and a difference fails naming both images. Off CI
/// another image, or no image at all, prints differences as advisory.
#[test]
fn the_host_image_decides_how_drift_is_treated() {
    let mac = RECORDING_IMAGES.iter().find(|image| image.platform == "macos").unwrap();
    for on_ci in [true, false] {
        let recorded = drift_policy("macos", &host(Some(mac.version), Some("arm64"), on_ci));
        assert_eq!(recorded, DriftPolicy::Exact);
        assert_eq!(recorded.note(), None);
        assert_eq!(recorded.judge(None), DriftVerdict::Holds);
        assert_eq!(recorded.judge(Some("moved")), DriftVerdict::Fails("moved".into()));
    }

    let other_ci = drift_policy("macos", &host(Some("20990101.1"), Some(mac.arch), true));
    assert!(
        other_ci.note().is_some_and(|note| note.contains("macos14 20990101.1")),
        "{other_ci:?}"
    );
    assert_eq!(
        other_ci.judge(None),
        DriftVerdict::Holds,
        "identical rows pass on another CI image"
    );
    let differing = other_ci.judge(Some("recorded [20, 20], live [19, 17]"));
    assert!(
        matches!(&differing, DriftVerdict::Fails(reason)
            if reason.starts_with("re-record the helper rows on macos14 20990101.1 ARM64")
                && reason.contains(mac.image)
                && reason.contains("live [19, 17]")),
        "{differing:?}"
    );

    for local in [host(None, None, false), host(Some("20990101.1"), Some(mac.arch), false)] {
        let policy = drift_policy("macos", &local);
        assert_eq!(policy, DriftPolicy::Advisory);
        assert_eq!(policy.judge(None), DriftVerdict::Holds);
        assert!(matches!(policy.judge(Some("moved")), DriftVerdict::Advisory(_)));
    }
}
