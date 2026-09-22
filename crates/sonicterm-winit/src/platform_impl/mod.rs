// Modified by SonicTerm: select only Windows, macOS, and Linux desktop backends.
use crate::monitor::{MonitorHandle as RootMonitorHandle, VideoModeHandle as RootVideoModeHandle};
use crate::window::Fullscreen as RootFullscreen;

#[cfg(any(x11_platform, wayland_platform))]
mod linux;
#[cfg(macos_platform)]
mod macos;
#[cfg(windows_platform)]
mod windows;

#[cfg(any(x11_platform, wayland_platform))]
use linux as platform;
#[cfg(macos_platform)]
use macos as platform;
#[cfg(windows_platform)]
use windows as platform;

pub use self::platform::*;

/// Helper for converting between platform-specific and generic
/// [`VideoModeHandle`]/[`MonitorHandle`]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fullscreen {
    Exclusive(VideoModeHandle),
    Borderless(Option<MonitorHandle>),
}

impl From<RootFullscreen> for Fullscreen {
    fn from(f: RootFullscreen) -> Self {
        match f {
            RootFullscreen::Exclusive(mode) => Self::Exclusive(mode.video_mode),
            RootFullscreen::Borderless(Some(handle)) => Self::Borderless(Some(handle.inner)),
            RootFullscreen::Borderless(None) => Self::Borderless(None),
        }
    }
}

impl From<Fullscreen> for RootFullscreen {
    fn from(f: Fullscreen) -> Self {
        match f {
            Fullscreen::Exclusive(video_mode) => {
                Self::Exclusive(RootVideoModeHandle { video_mode })
            },
            Fullscreen::Borderless(Some(inner)) => {
                Self::Borderless(Some(RootMonitorHandle { inner }))
            },
            Fullscreen::Borderless(None) => Self::Borderless(None),
        }
    }
}

#[cfg(not(any(windows_platform, macos_platform, x11_platform, wayland_platform)))]
compile_error!("This winit desktop subset requires Windows, macOS, or Linux with X11 or Wayland");
