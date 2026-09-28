//! Warm-window pool sizing and spawn rules.

use super::*;

/// How many pre-created windows to hold ready, from the configured request.
///
/// Prewarmed windows trade idle memory for tear-out latency, so the request is
/// capped and reduced when that trade is a poor one.
#[must_use]
pub fn warm_window_pool_target(configured: u8, software_rendering: bool) -> usize {
    if configured == 0 {
        // When: `configured` opts out of prewarming, so no window is held and each
        // tear-out pays full creation cost.
        return 0;
    }
    if software_rendering {
        // When: `software_rendering` makes every spare window a full CPU surface,
        // so hold one rather than the configured count.
        return 1;
    }
    usize::from(configured).min(WARM_WINDOW_POOL_MAX)
}

/// Whether another window should be prewarmed into the pool right now.
#[must_use]
pub fn warm_window_pool_should_spawn(
    current_len: usize,
    configured: u8,
    software_rendering: bool,
) -> bool {
    current_len < warm_window_pool_target(configured, software_rendering)
}

/// Whether the pool may prewarm another window right now.
///
/// A stopped device refuses every renderer creation, so prewarming would create
/// and drop a hidden native window on every event-loop pass. While the main
/// window's device accepts no GPU work, nothing is prewarmed.
#[must_use]
pub fn warm_window_pool_may_spawn(
    device_accepts_gpu_work: bool,
    current_len: usize,
    configured: u8,
    software_rendering: bool,
) -> bool {
    device_accepts_gpu_work
        && warm_window_pool_should_spawn(current_len, configured, software_rendering)
}

pub struct WarmWindow {
    pub window: Arc<Window>,
    pub renderer: GpuRenderer,
    pub created_at: Instant,
}

#[cfg(test)]
#[path = "warm_window_pool_tests.rs"]
mod warm_window_pool_tests;
