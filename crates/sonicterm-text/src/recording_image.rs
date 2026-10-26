//! The CI runner image each platform's helper rows were recorded on, and how the working-set step
//! treats a host on another image.

/// The runner image one platform's helper rows were measured on, as its job log printed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingImage {
    /// The platform as the rows name it: `macos` or `windows`.
    pub platform: &'static str,
    /// The runner image name.
    pub image: &'static str,
    /// The runner's `ImageVersion`.
    pub version: &'static str,
    /// The runner's `RUNNER_ARCH`.
    pub arch: &'static str,
    /// The CI job whose log names the image.
    pub job: u64,
}

/// The image every platform's helper rows were recorded on; re-recording the rows updates it.
pub const RECORDING_IMAGES: &[RecordingImage] = &[
    RecordingImage {
        platform: "macos",
        image: "macos-14-arm64",
        version: "20260831.0302.1",
        arch: "ARM64",
        job: 121_555_774_360,
    },
    RecordingImage {
        platform: "windows",
        image: "windows-2025-vs2026",
        version: "20261021.278.1",
        arch: "X64",
        job: 121_556_063_193,
    },
];

/// The host the working-set step runs on, from the CI environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostImage {
    /// `ImageOS`, set only on a CI runner.
    pub os: Option<String>,
    /// `ImageVersion`, set only on a CI runner.
    pub version: Option<String>,
    /// `RUNNER_ARCH`, set only on a CI runner.
    pub arch: Option<String>,
    /// Whether `GITHUB_ACTIONS` is `true`.
    pub ci: bool,
}

impl HostImage {
    /// Read `ImageOS`, `ImageVersion`, `RUNNER_ARCH` and `GITHUB_ACTIONS` from the environment.
    #[must_use]
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        Self {
            os: var("ImageOS"),
            version: var("ImageVersion"),
            arch: var("RUNNER_ARCH"),
            ci: var("GITHUB_ACTIONS").as_deref() == Some("true"),
        }
    }
}

/// How the working-set step treats a helper measurement that differs from its recorded row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftPolicy {
    /// The host is the recording image, or none is recorded: a difference fails the step.
    Exact,
    /// A local host on another image: a difference prints as advisory.
    Advisory,
    /// A CI host on another image: the step fails, naming the image to re-record on.
    Rerecord(String),
}

/// The drift policy for `platform` on `host`. Completeness, raster failures and the live fit stay
/// mandatory under every policy; only the comparison with the recorded row depends on the image.
#[must_use]
pub fn drift_policy(platform: &str, host: &HostImage) -> DriftPolicy {
    let Some(recorded) = RECORDING_IMAGES.iter().find(|image| image.platform == platform) else {
        // When: `RECORDING_IMAGES` names no image for `platform`, no recorded rows can drift.
        return DriftPolicy::Exact;
    };
    let same_image = host.version.as_deref() == Some(recorded.version)
        && host.arch.as_deref().is_some_and(|arch| arch.eq_ignore_ascii_case(recorded.arch));
    if same_image {
        DriftPolicy::Exact
    } else if host.ci {
        // When: `host.ci` is set on another image, the rows cannot vouch for it until re-recorded.
        let named = |value: &Option<String>| value.clone().unwrap_or_else(|| "?".to_owned());
        DriftPolicy::Rerecord(format!(
            "re-record the helper rows on {} {} {}: the {platform} rows were recorded on {} {} {} \
             (job {})",
            named(&host.os),
            named(&host.version),
            named(&host.arch),
            recorded.image,
            recorded.version,
            recorded.arch,
            recorded.job
        ))
    } else {
        // When: neither `same_image` nor `host.ci`, a local host on another image gets advisories.
        DriftPolicy::Advisory
    }
}

#[cfg(test)]
#[path = "recording_image_tests.rs"]
mod recording_image_tests;
