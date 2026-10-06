//! The perf-end glyph completeness checkpoint: a certificate stored at each presented `Full` frame,
//! read back only while the scene and the glyph atlas still match it.
//!
//! The renderer's latest missing-character lists describe one assembly, and a partial frame replaces
//! them, so they are not a census of the scene. A `Full` frame assembles every visible row and its
//! chrome, so its lists are. The certificate keeps that frame's scene key (its `FrameKey` without the
//! pane revision and dirty generation, which only mark dirt), the atlas content stamp and dimensions at
//! present, and the distinct missing characters. A later presentation of the same scene and atlas adds
//! its own missing characters, so a character drawn as tofu after the `Full` frame is never hidden; any
//! other presentation, or a changed atlas, makes the reading unavailable until the next `Full` frame.

use std::collections::BTreeSet;

/// Why a checkpoint has no counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletenessUnavailable {
    /// No `Full` frame has presented since the renderer was created.
    NoCertificate,
    /// The presented scene is no longer the one the certificate describes.
    SceneChanged,
    /// The glyph atlas's content, device, allocation or dimensions changed since the certificate.
    AtlasChanged,
}

impl CompletenessUnavailable {
    /// The reason as the comparison reports it.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoCertificate => "no certificate",
            Self::SceneChanged => "scene changed",
            Self::AtlasChanged => "atlas changed",
        }
    }
}

/// Distinct missing characters of a certified scene, terminal and chrome counted separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletenessCounts {
    /// Distinct non-whitespace terminal characters drawn as tofu or not drawn.
    pub missing_terminal: usize,
    /// Distinct non-whitespace chrome characters drawn as tofu or dropped.
    pub missing_chrome: usize,
    /// The glyph atlas's width and height at the certificate's `Full` frame.
    pub atlas_dims: (u32, u32),
}

/// One checkpoint reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletenessCheckpoint {
    /// The certificate still describes the presented scene and the current atlas.
    Certified(CompletenessCounts),
    /// No counts; the reason says why.
    Unavailable(CompletenessUnavailable),
}

/// What a `Full` frame certified, generic over the scene and atlas stamp so the rules are tested
/// without a device.
#[derive(Debug, Clone)]
pub(crate) struct Certificate<Scene, Stamp> {
    scene: Scene,
    stamp: Stamp,
    atlas_dims: (u32, u32),
    missing_terminal: BTreeSet<char>,
    missing_chrome: BTreeSet<char>,
    /// Set by a later presentation that did not match; kept for diagnostics only.
    superseded: Option<CompletenessUnavailable>,
}

/// One presented frame, as the certificate reads it.
pub(crate) struct Presented<'frame, Scene, Stamp> {
    /// Whether the frame assembled every row (`RenderMode::Full`).
    pub(crate) full: bool,
    pub(crate) scene: Scene,
    pub(crate) stamp: Stamp,
    pub(crate) atlas_dims: (u32, u32),
    pub(crate) missing_terminal: &'frame [char],
    pub(crate) missing_chrome: &'frame [char],
}

/// The distinct non-whitespace characters of one missing list; whitespace is intentionally blank.
fn distinct(chars: &[char]) -> impl Iterator<Item = char> + '_ {
    chars.iter().copied().filter(|character| !character.is_whitespace())
}

/// Fold one presented frame into the certificate. A `Full` frame replaces it. A partial frame of the
/// same scene and atlas adds its missing characters; any other partial frame supersedes it, because it
/// drew characters the certificate never saw, and only the next `Full` frame certifies again.
pub(crate) fn record_presented<Scene: PartialEq, Stamp: PartialEq>(
    certificate: &mut Option<Certificate<Scene, Stamp>>,
    frame: Presented<'_, Scene, Stamp>,
) {
    if frame.full {
        // When: the frame assembled every row and its chrome, its lists are the scene's census.
        *certificate = Some(Certificate {
            missing_terminal: distinct(frame.missing_terminal).collect(),
            missing_chrome: distinct(frame.missing_chrome).collect(),
            scene: frame.scene,
            stamp: frame.stamp,
            atlas_dims: frame.atlas_dims,
            superseded: None,
        });
        return;
    }
    let Some(current) = certificate.as_mut() else {
        // When: `certificate` is None, no `Full` frame has presented, so a partial frame has nothing to add to.
        return;
    };
    if current.superseded.is_some() {
        // When: `superseded` is already set, the certificate stays unavailable until the next `Full` frame.
        return;
    }
    if current.stamp != frame.stamp || current.atlas_dims != frame.atlas_dims {
        // The frame drew from another atlas than the certificate's.
        current.superseded = Some(CompletenessUnavailable::AtlasChanged);
    } else if current.scene != frame.scene {
        // When: the `scene` differs, the frame drew characters the certificate never saw.
        current.superseded = Some(CompletenessUnavailable::SceneChanged);
    } else {
        // When: neither differs, the frame's missing characters join the census, never replace it.
        current.missing_terminal.extend(distinct(frame.missing_terminal));
        current.missing_chrome.extend(distinct(frame.missing_chrome));
    }
}

/// Read the checkpoint against the renderer's retained scene and current atlas. The atlas is compared
/// first, so an attempt that changed it reads `atlas changed` even after it cleared the retained scene.
pub(crate) fn read_checkpoint<Scene: PartialEq, Stamp: PartialEq>(
    certificate: Option<&Certificate<Scene, Stamp>>,
    retained_scene: Option<&Scene>,
    stamp: &Stamp,
    atlas_dims: (u32, u32),
) -> CompletenessCheckpoint {
    let Some(certificate) = certificate else {
        // When: `certificate` is None, no `Full` frame has presented, so there is no census to report.
        return CompletenessCheckpoint::Unavailable(CompletenessUnavailable::NoCertificate);
    };
    if certificate.stamp != *stamp || certificate.atlas_dims != atlas_dims {
        // When: the current `stamp` or `atlas_dims` differ, the atlas changed since the certificate.
        return CompletenessCheckpoint::Unavailable(CompletenessUnavailable::AtlasChanged);
    }
    if let Some(reason) = certificate.superseded {
        // When: `superseded` holds a reason, a later frame already made the certificate stale.
        return CompletenessCheckpoint::Unavailable(reason);
    }
    if retained_scene != Some(&certificate.scene) {
        // When: the renderer retains another scene, or none after a failed attempt, the census is stale.
        return CompletenessCheckpoint::Unavailable(CompletenessUnavailable::SceneChanged);
    }
    CompletenessCheckpoint::Certified(CompletenessCounts {
        missing_terminal: certificate.missing_terminal.len(),
        missing_chrome: certificate.missing_chrome.len(),
        atlas_dims: certificate.atlas_dims,
    })
}

#[cfg(test)]
#[path = "completeness_tests.rs"]
mod completeness_tests;
