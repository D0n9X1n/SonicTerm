//! Display-link pacing for deferred hardware streaming.
//!
//! A window whose streaming frame is deferred on the hardware path is admitted on a display-link
//! tick, with a two-period fallback ceiling, instead of one period after its last render. The link
//! runs only while such an admission is pending. Every other deferral keeps its timer. The source is
//! a trait so the rules run against a fake on every platform; a window with no source installed
//! keeps the timer path unchanged.

use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use winit::window::WindowId;

use super::{redraw::WindowRedrawState, App, WindowState};

/// A per-window frame-timing source the App starts and stops.
pub(crate) trait DisplayLinkSource {
    /// Start or pause the source's ticks.
    fn set_running(&mut self, running: bool);
    /// Ask the source to tick once per `period`, the window's monitor refresh period.
    fn set_preferred_period(&mut self, period: Duration);
}

/// How a pending admission is paced, fixed at the deferral that stored it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PacingMode {
    /// One period from the pacing clock, a surface timeout or a contention floor.
    Timer,
    /// A display-link tick, or the two-period fallback ceiling when no tick comes.
    Link,
}

/// A window's installed display-link source and the generation its native link was started for.
#[derive(Default)]
pub(crate) struct WindowDisplayLink {
    /// The installed source; `None` keeps the window on the timer path.
    pub(crate) source: Option<Box<dyn DisplayLinkSource>>,
    /// The generation the native link was started for, or `None` while it is paused.
    pub(crate) native_generation: Option<u64>,
}

impl WindowRedrawState {
    /// Store `mode` for the pending admission unless a mode is already stored; a stored mode is kept.
    pub(super) fn store_pacing(&mut self, mode: PacingMode) {
        if self.pacing.is_none() {
            self.pacing = Some(mode);
        }
    }

    /// Cancel link pacing on a pause, suppression or restart: a newer generation rejects queued
    /// ticks, readiness is dropped, and a stored `Link` is cleared; a stored `Timer` is not the link's.
    // Ordering: link_generation Release publishes the bump before the native target's Acquire read posts a tick.
    pub(super) fn invalidate_link_pacing(&mut self) {
        self.link_generation.fetch_add(1, Ordering::Release);
        self.link_live = None;
        self.link_permit = None;
        if self.pacing == Some(PacingMode::Link) {
            self.pacing = None;
        }
    }

    /// Whether an accepted tick of the running interval is still unspent.
    pub(super) fn link_permit_valid(&self) -> bool {
        self.link_permit.is_some() && self.link_permit == self.link_live
    }
}

impl WindowState {
    /// Tell the installed source the window's current monitor period, if a source is installed.
    pub(super) fn push_preferred_period(&mut self) {
        let period = self.redraw.monitor_period;
        if let Some(source) = self.display_link.source.as_mut() {
            source.set_preferred_period(period);
        }
    }

    /// The fallback ceiling of a `Link` admission: two periods after the pacing clock.
    pub(super) fn link_ceiling(&self, period: Duration, software: bool) -> Instant {
        self.pacing_clock(software) + period * 2
    }

    /// Whether this window may run its link and accept ticks now, apart from its pending mode.
    pub(super) fn link_eligible(&self, software: bool) -> bool {
        !self.redraw.sync_hold() && !software && self.frame_deadlines_allowed()
    }

    /// The Frame deadline of a deferred window: the timer rule's first admissible instant, raised to
    /// the fallback ceiling while a `Link` admission waits for a tick.
    pub(super) fn frame_deadline(&self, period: Duration, software: bool) -> Instant {
        let floor = self.redraw_not_before(period, software);
        if self.redraw.pacing == Some(PacingMode::Link)
            && !self.redraw.sync_hold()
            && !self.redraw.link_permit_valid()
        {
            floor.max(self.link_ceiling(period, software))
        } else {
            // When: `pacing` is not `Link`, `sync_hold` holds, or `link_permit_valid` is true, the floor decides.
            floor
        }
    }

    /// Reconcile this window's native link with its logical pacing state. A pause stops only the
    /// generation it was started for and never writes `pacing`; a start always bumps the generation.
    // Ordering: link_generation Release bumps are published before the native target's Acquire read.
    pub(super) fn sync_display_link(&mut self, software: bool) {
        let should_run = self.display_link.source.is_some()
            && self.redraw.pacing == Some(PacingMode::Link)
            && self.link_eligible(software);
        if let Some(native) = self.display_link.native_generation {
            if !should_run || self.redraw.link_live != Some(native) {
                // Nothing link-paced is pending, or `native` belongs to an invalidated interval: pause it.
                if let Some(source) = self.display_link.source.as_mut() {
                    source.set_running(false);
                }
                self.display_link.native_generation = None;
                if self.redraw.link_live == Some(native) {
                    // The paused interval is the live one, so its readiness ends with it.
                    self.redraw.link_generation.fetch_add(1, Ordering::Release);
                    self.redraw.link_live = None;
                    self.redraw.link_permit = None;
                }
            }
        }
        if should_run && self.redraw.link_live.is_none() {
            // The generation is bumped before the link is unpaused, so every tick carries the new one.
            let generation = self.redraw.link_generation.fetch_add(1, Ordering::Release) + 1;
            self.redraw.link_live = Some(generation);
            self.display_link.native_generation = Some(generation);
            if let Some(source) = self.display_link.source.as_mut() {
                source.set_running(true);
            }
        }
    }
}

impl App {
    /// Install `source` as window `id`'s display link and set its preferred period; it starts paused.
    pub(super) fn install_display_link(
        &mut self,
        id: WindowId,
        source: Box<dyn DisplayLinkSource>,
    ) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.display_link.source = Some(source);
            window.display_link.native_generation = None;
            window.push_preferred_period();
        }
    }

    /// Install window `id`'s native display link when it has a native view and the App has an
    /// event-loop proxy. Only macOS installs one, and only from macOS 14; every other window keeps
    /// the timer path. A window that already has a source keeps it.
    pub(super) fn install_native_display_link(&mut self, id: WindowId) {
        if self.windows.get(&id).is_none_or(|window| window.display_link.source.is_some()) {
            // When: `id` is gone or already has a source, a second link would duplicate its ticks.
            return;
        }
        if let Some(source) = self.native_display_link_source(id) {
            self.install_display_link(id, source);
        }
    }

    /// The native source for window `id`: a paused `NSView.displayLink`, or `None`.
    #[cfg(target_os = "macos")]
    fn native_display_link_source(&self, id: WindowId) -> Option<Box<dyn DisplayLinkSource>> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let mtm = objc2::MainThreadMarker::new()?;
        let proxy = self.event_loop_proxy.clone()?;
        let window = self.windows.get(&id)?;
        let handle = window.window.as_ref()?.window_handle().ok()?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            // When: the handle is not AppKit's, there is no NSView to take a link from.
            return None;
        };
        let view =
            // SAFETY: appkit.ns_view is winit's live NSView for this window; it outlives the borrow, used on the main thread.
            unsafe { appkit.ns_view.cast::<objc2_app_kit::NSView>().as_ref() };
        let generation = Arc::clone(&window.redraw.link_generation);
        let link = native::NativeDisplayLink::new(mtm, view, id, proxy, generation)?;
        Some(Box::new(link))
    }

    /// Windows and Linux install no link, so every window keeps the timer path.
    #[cfg(not(target_os = "macos"))]
    fn native_display_link_source(&self, _id: WindowId) -> Option<Box<dyn DisplayLinkSource>> {
        None
    }

    /// Start or pause every window's display link from its pending admission's mode.
    pub(super) fn sync_display_links(&mut self) {
        let software = self.software_render_degrade;
        for window in self.windows.values_mut() {
            window.sync_display_link(software);
        }
    }

    /// Accept a display-link tick only for the window's live generation and a pending `Link` admission:
    /// it stores a permit, counts the tick and requests one frame. Any other tick is dropped.
    pub(super) fn handle_display_link_tick(
        &mut self,
        id: WindowId,
        generation: u64,
        target: Instant,
    ) {
        let software = self.software_render_degrade;
        let now = self.dispatch_now();
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` closed after its tick was queued, no other window may take the tick.
            tracing::trace!(
                ?id,
                generation,
                ?target,
                ?now,
                "display-link tick for a closed window"
            );
            return;
        };
        if window.redraw.link_live != Some(generation)
            || window.redraw.pacing != Some(PacingMode::Link)
            || !window.link_eligible(software)
        {
            // When: the tick's interval is stale, nothing link-paced is pending, or the window is held back.
            tracing::trace!(?id, generation, ?target, ?now, "display-link tick rejected");
            return;
        }
        window.redraw.link_permit = Some(generation);
        if let Some(counters) = window.redraw.frame_counters.as_deref_mut() {
            // the App's gate is on, each accepted tick is counted.
            counters.display_link_ticks += 1;
        }
        if !window.redraw.request_in_flight {
            window.redraw.request_in_flight = true;
            window.request_window_redraw();
        }
    }

    /// Set the software-render degrade flag; turning it on invalidates every window's link pacing,
    /// because the software path never runs a link.
    pub(super) fn set_software_render_degrade(&mut self, degrade: bool) {
        let turning_on = degrade && !self.software_render_degrade;
        self.software_render_degrade = degrade;
        if turning_on {
            for window in self.windows.values_mut() {
                window.redraw.invalidate_link_pacing();
                // The software path takes no fast retry; every ask and token resolves now.
                window.redraw.suppress_yield(super::parser_yield::YieldLoss::Software);
                window.redraw.resolve_yield_ask();
            }
        }
    }
}

/// Convert a display link's `target` time, in the media-time seconds `now_media` is read in, to an
/// `Instant` relative to `now`; a non-finite or past target maps to `now`. For diagnostics only.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the native tick target reads it")
)]
pub(super) fn target_instant(now: Instant, target_media: f64, now_media: f64) -> Instant {
    let ahead_s = target_media - now_media;
    if ahead_s.is_finite() && ahead_s > 0.0 {
        now + Duration::from_secs_f64(ahead_s)
    } else {
        // When: `ahead_s` is not a finite positive lead, the target is treated as now.
        now
    }
}

/// The macOS source: one `NSView.displayLink` per window, driven by `setPaused`.
#[cfg(target_os = "macos")]
pub(super) mod native {
    use std::{
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    use objc2::{
        define_class, msg_send, rc::Retained, runtime::NSObject, sel, DefinedClass,
        MainThreadMarker, MainThreadOnly,
    };
    use objc2_app_kit::NSView;
    use objc2_foundation::{NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes};
    use objc2_quartz_core::{CACurrentMediaTime, CADisplayLink, CAFrameRateRange};
    use winit::{event_loop::EventLoopProxy, window::WindowId};

    use super::{target_instant, DisplayLinkSource};
    use crate::app::UserEvent;

    /// What the tick target holds: the window it ticks for, a proxy clone and the shared generation.
    pub(crate) struct TickTargetState {
        /// The window whose link this target serves.
        window_id: WindowId,
        /// The App's event-loop proxy; the only way a tick reaches the App.
        proxy: EventLoopProxy<UserEvent>,
        /// The window's link generation, which the App bumps on every start and invalidation.
        generation: Arc<AtomicU64>,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and the target overrides no NSObject method.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SonicTermDisplayLinkTarget"]
        #[ivars = TickTargetState]
        pub(crate) struct TickTarget;

        // SAFETY: NSObjectProtocol has no safety requirements beyond being an NSObject subclass.
        unsafe impl NSObjectProtocol for TickTarget {}

        impl TickTarget {
            /// The link's selector: post the live generation as a tick. It never touches the App.
            // Ordering: generation Acquire pairs with the App's Release bump, so the tick carries the newest interval.
            #[unsafe(method(displayLinkFired:))]
            fn display_link_fired(&self, link: &CADisplayLink) {
                let state = self.ivars();
                let generation = state.generation.load(Ordering::Acquire);
                let target = target_instant(Instant::now(), link.targetTimestamp(), CACurrentMediaTime());
                // A closed event loop drops the tick; nothing is left to pace.
                let _ = state.proxy.send_event(UserEvent::DisplayLinkTick {
                    window_id: state.window_id,
                    generation,
                    target,
                });
            }
        }
    );

    impl TickTarget {
        /// A target for `window_id` that posts through `proxy` and reads `generation` when fired.
        fn new(
            mtm: MainThreadMarker,
            window_id: WindowId,
            proxy: EventLoopProxy<UserEvent>,
            generation: Arc<AtomicU64>,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(TickTargetState { window_id, proxy, generation });
            // SAFETY: `this` is a freshly allocated TickTarget with its ivars set, and NSObject's
            // designated initializer `init` runs exactly once on it.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// A window's native display link: created paused on the main run loop in common modes.
    pub(crate) struct NativeDisplayLink {
        /// The link; it retains `target` until invalidated.
        link: Retained<CADisplayLink>,
        /// The tick target, kept so a test can fire it by hand.
        target: Retained<TickTarget>,
    }

    impl NativeDisplayLink {
        /// Create a paused link for `view`, or `None` before macOS 14, where the window keeps the timer.
        pub(crate) fn new(
            mtm: MainThreadMarker,
            view: &NSView,
            window_id: WindowId,
            proxy: EventLoopProxy<UserEvent>,
            generation: Arc<AtomicU64>,
        ) -> Option<Self> {
            if !objc2::available!(macos = 14.0) {
                // When: `available!` reports macos below 14.0, NSView.displayLink is missing; the timer path stays.
                return None;
            }
            let target = TickTarget::new(mtm, window_id, proxy, generation);
            let link =
                // SAFETY: `target` is a live TickTarget whose `displayLinkFired:` takes the one CADisplayLink argument.
                unsafe { view.displayLinkWithTarget_selector(&target, sel!(displayLinkFired:)) };
            link.setPaused(true);
            // SAFETY: `mtm` proves the main thread, and NSRunLoopCommonModes is a process-lifetime constant.
            unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
            Some(Self { link, target })
        }

        /// Whether the link is paused now.
        pub(crate) fn is_paused(&self) -> bool {
            self.link.isPaused()
        }

        /// The link's preferred frame-rate range: minimum, maximum and preferred.
        pub(crate) fn preferred_range(&self) -> (f32, f32, f32) {
            let range = self.link.preferredFrameRateRange();
            (range.minimum, range.maximum, range.preferred)
        }

        /// Fire the tick target by hand, as the link does on a display refresh.
        pub(crate) fn fire_target(&self) {
            let () =
                // SAFETY: TickTarget implements `displayLinkFired:` for one CADisplayLink, and `link` is live.
                unsafe { msg_send![&*self.target, displayLinkFired: &*self.link] };
        }

        /// A weak handle on the tick target, to observe its release.
        pub(crate) fn target_weak(&self) -> objc2::rc::Weak<TickTarget> {
            objc2::rc::Weak::from_retained(&self.target)
        }
    }

    impl DisplayLinkSource for NativeDisplayLink {
        fn set_running(&mut self, running: bool) {
            self.link.setPaused(!running);
        }

        fn set_preferred_period(&mut self, period: Duration) {
            let rate = 1.0 / period.as_secs_f32();
            if rate.is_finite() {
                self.link.setPreferredFrameRateRange(CAFrameRateRange::new(rate, rate, rate));
            }
        }
    }

    // Lifecycle: dropping NativeDisplayLink calls link.invalidate, which removes it from every run loop and releases the target.
    impl Drop for NativeDisplayLink {
        fn drop(&mut self) {
            self.link.invalidate();
        }
    }
}

/// What the native display-link probe observed.
#[doc(hidden)]
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy)]
pub struct NativeDisplayLinkReport {
    /// Whether the link was paused right after creation.
    pub created_paused: bool,
    /// The preferred frame-rate range after installation: minimum, maximum and preferred, in Hz.
    pub range: (f32, f32, f32),
    /// Whether `set_running(true)` unpaused it.
    pub running_after_start: bool,
    /// Whether `set_running(false)` paused it again.
    pub paused_after_stop: bool,
    /// Whether dropping the handle invalidated the link and released its tick target.
    pub target_released_on_drop: bool,
}

/// Test seam: build a native link on a bare `NSView`, set `period`, round-trip pause and unpause,
/// store `live` in `generation` (Release), fire the target by hand and drop the handle. Must run on
/// the main thread with `proxy`'s event loop alive; `None` before macOS 14 or off the main thread.
// Ordering: generation Release publishes `live` before the hand-fired target's Acquire read.
#[doc(hidden)]
#[cfg(target_os = "macos")]
pub fn probe_native_display_link(
    proxy: winit::event_loop::EventLoopProxy<super::UserEvent>,
    window_id: WindowId,
    generation: &Arc<std::sync::atomic::AtomicU64>,
    live: u64,
    period: Duration,
) -> Option<NativeDisplayLinkReport> {
    let mtm = objc2::MainThreadMarker::new()?;
    let view = objc2_app_kit::NSView::new(mtm);
    let mut link =
        native::NativeDisplayLink::new(mtm, &view, window_id, proxy, Arc::clone(generation))?;
    let created_paused = link.is_paused();
    link.set_preferred_period(period);
    let range = link.preferred_range();
    link.set_running(true);
    let running_after_start = !link.is_paused();
    link.set_running(false);
    let paused_after_stop = link.is_paused();
    generation.store(live, Ordering::Release);
    link.fire_target();
    let target = link.target_weak();
    objc2::rc::autoreleasepool(|_| drop(link));
    let target_released_on_drop = target.load().is_none();
    Some(NativeDisplayLinkReport {
        created_paused,
        range,
        running_after_start,
        paused_after_stop,
        target_released_on_drop,
    })
}

/// The shared generation a fresh window's link starts from.
pub(super) fn new_link_generation() -> Arc<std::sync::atomic::AtomicU64> {
    Arc::new(std::sync::atomic::AtomicU64::new(0))
}

/// A recording fake for tests: what it was told, in order.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct FakeLinkLog {
    /// Whether the fake is running now.
    pub(crate) running: bool,
    /// Every `set_running` argument, in order.
    pub(crate) calls: Vec<bool>,
    /// Every `set_preferred_period` argument, in order.
    pub(crate) periods: Vec<Duration>,
}

/// A fake display-link source that records into a shared log.
#[cfg(test)]
pub(crate) struct FakeLink(pub(crate) std::rc::Rc<std::cell::RefCell<FakeLinkLog>>);

#[cfg(test)]
impl DisplayLinkSource for FakeLink {
    fn set_running(&mut self, running: bool) {
        let mut log = self.0.borrow_mut();
        log.running = running;
        log.calls.push(running);
    }

    fn set_preferred_period(&mut self, period: Duration) {
        self.0.borrow_mut().periods.push(period);
    }
}

#[cfg(test)]
#[path = "display_link_tests.rs"]
mod display_link_tests;
