//! Accessibility: one accesskit adapter per terminal window, installed dormant before the window
//! is first shown, and the status line's tree, keyboard focus and announcements.
//!
//! State is kept per window id in `App::accessibility`, warm spares included, so a request that
//! reaches a spare before promotion is served once it is a terminal window. While no assistive
//! technology is connected nothing here builds a tree, requests a frame or reads a parser. Once one
//! connects, `App::flush_accessibility` runs once per dispatch, after the redraws, and sends a full
//! tree only when what a screen reader can read changed. No redraw path builds or sends a tree.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use accesskit::{
    Action as AccessAction, ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler,
    Live, Node, NodeId, Rect, Role, TreeId, TreeInfo, TreeUpdate,
};
use sonicterm_cfg::status_line::{ResolvedAccessibility, Sensitivity, StatusSegmentId};
use sonicterm_types::{ResourceAmount, ResourceClass};
use sonicterm_ui::status_line::{
    accessible_strip, format_duration, AccessibleRole, AccessibleStrip, CommittedSegment,
    SegmentClick, StatusLineView, StatusTone,
};
use sonicterm_ui::tabs::{MarkKind, TabId};
use winit::event::{Ime, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use super::redraw::RedrawCause;
use super::{App, UserEvent};

/// What an assistive technology asked of a terminal window's adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessibilityRequest {
    /// An assistive technology connected and wants the full tree.
    InitialTree,
    /// The last assistive technology disconnected.
    Deactivated,
    /// Move focus to a node; never activates anything.
    Focus {
        /// The node.
        node: u64,
        /// What the node named when the adapter's handler resolved it.
        target: Option<RequestTarget>,
    },
    /// Activate a node.
    Click {
        /// The node.
        node: u64,
        /// What the node named when the adapter's handler resolved it.
        target: Option<RequestTarget>,
    },
    /// Any other action, which the strip does not support.
    Other,
}

/// What a Focus or Click names, resolved by the adapter's handler from the targets the App last
/// published: the segment, the pane its click acts on, what clicking runs, and the reload and resize
/// epochs. A request runs only while the window still exposes this exact target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestTarget {
    /// The segment.
    id: StatusSegmentId,
    /// The focused pane a click acts on.
    pane: Option<u64>,
    /// What clicking runs, as a fingerprint; `None` is inert.
    action: Option<u64>,
    /// The strip settings generation.
    settings: u64,
    /// The window's resize and scale epoch.
    epoch: u64,
}

/// The adapter's callbacks: each posts a request to the event loop, and an action carries the
/// target its node named in the table the App published with the tree the adapter holds.
#[derive(Clone)]
struct AdapterHandlers {
    /// The window whose adapter calls back.
    window_id: WindowId,
    /// Where requests go.
    proxy: EventLoopProxy<UserEvent>,
    /// The targets of the tree the adapter holds, by node id.
    targets: Arc<Mutex<HashMap<u64, RequestTarget>>>,
}

impl AdapterHandlers {
    /// Post `request` for this window; a closed event loop has no window left to answer it.
    fn post(&self, request: AccessibilityRequest) {
        let _ =
            self.proxy.send_event(UserEvent::Accessibility { window_id: self.window_id, request });
    }
}

impl ActivationHandler for AdapterHandlers {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        // The tree is built on the event loop by the next flush, never on the adapter's thread.
        self.post(AccessibilityRequest::InitialTree);
        None
    }
}

/// The target `node` names in the table the App last published; `None` when it names none, or when
/// a panic poisoned the table, in which case the request is dropped.
fn resolve_target(
    targets: &Mutex<HashMap<u64, RequestTarget>>,
    node: u64,
) -> Option<RequestTarget> {
    targets.lock().ok()?.get(&node).copied()
}

/// A fingerprint of what clicking runs, so an exposed segment and a target stay plain data.
fn action_fingerprint(click: &SegmentClick) -> u64 {
    let mut hasher = DefaultHasher::new();
    format!("{click:?}").hash(&mut hasher);
    hasher.finish()
}

/// The target a request naming `exposed` resolves to in `structure`.
fn target_of(structure: &Structure, exposed: &Exposed) -> RequestTarget {
    RequestTarget {
        id: exposed.id,
        pane: structure.pane,
        action: exposed.action,
        settings: structure.settings,
        epoch: structure.epoch,
    }
}

impl ActionHandler for AdapterHandlers {
    fn do_action(&mut self, request: ActionRequest) {
        let node = request.target_node.0;
        // Resolved now, against the targets published with the tree the adapter holds.
        let target = resolve_target(&self.targets, node);
        self.post(match request.action {
            AccessAction::Focus => AccessibilityRequest::Focus { node, target },
            AccessAction::Click => AccessibilityRequest::Click { node, target },
            _ => AccessibilityRequest::Other,
        });
    }
}

impl DeactivationHandler for AdapterHandlers {
    fn deactivate_accessibility(&mut self) {
        self.post(AccessibilityRequest::Deactivated);
    }
}

/// Bits every node id of a terminal window's tree carries, so the ids are a role constant.
const TERMINAL_WINDOW: u64 = 1 << 48;
/// Node kinds, in bits 32..48.
const KIND_ROOT: u64 = 1 << 32;
const KIND_STRIP: u64 = 2 << 32;
const KIND_ANNOUNCEMENT: u64 = 3 << 32;
const KIND_SEGMENT: u64 = 4 << 32;
const KIND_ABOUT_ROW: u64 = 5 << 32;
const KIND_ABOUT_COPY: u64 = 6 << 32;
/// The About window's Copy button.
pub(crate) const ABOUT_COPY_NODE: NodeId = NodeId(TERMINAL_WINDOW | KIND_ABOUT_COPY);
/// The window node.
pub(crate) const ROOT_NODE: NodeId = NodeId(TERMINAL_WINDOW | KIND_ROOT);
/// The status line group.
pub(crate) const STRIP_NODE: NodeId = NodeId(TERMINAL_WINDOW | KIND_STRIP);
/// The announcement node, present in every tree.
pub(crate) const ANNOUNCEMENT_NODE: NodeId = NodeId(TERMINAL_WINDOW | KIND_ANNOUNCEMENT);

/// The announcement node's text and urgency.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Announcement {
    /// What the screen reader says.
    pub(crate) text: String,
    /// Errors interrupt; everything else waits its turn.
    pub(crate) assertive: bool,
}

/// How bad the health segment is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Severity {
    /// Nothing is wrong.
    #[default]
    None,
    /// A warning.
    Warn,
    /// An error.
    Error,
}

/// The least time between two announcements, so a screen reader finishes one before the node's
/// text changes again.
const ANNOUNCE_SPACING: Duration = Duration::from_millis(500);

/// A transition waiting for its segment's minimum interval; only the latest per segment is kept.
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    /// The segment that changed.
    id: StatusSegmentId,
    /// What to say.
    text: String,
    /// Errors interrupt; everything else waits its turn.
    assertive: bool,
    /// When the transition happened.
    queued_at: Instant,
}

/// One exposed segment: what a request against it must still find.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Exposed {
    /// The segment.
    id: StatusSegmentId,
    /// Left edge, raster px.
    left_px: f32,
    /// Width, raster px.
    width: f32,
    /// What a click runs, as a fingerprint; `None` is inert.
    action: Option<u64>,
}

/// What a tree exposes to act on. A change republishes the targets requests resolve against.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Structure {
    /// The exposed segments, left to right.
    segments: Vec<Exposed>,
    /// The band's top and height, raster px; `None` while the strip is off or not exposed.
    band: Option<(f32, f32)>,
    /// The focused pane a click acts on.
    pane: Option<u64>,
    /// The window's resize and scale epoch: a change replaces the bounds before a frame commits.
    epoch: u64,
    /// The strip settings generation: a reload can change what a node's click does.
    settings: u64,
}

/// One terminal window's accessibility state, bounded by the strip's segment count.
#[derive(Default)]
pub(crate) struct WindowAccessibility {
    /// The platform adapter; `None` where no event loop could create one (headless tests).
    adapter: Option<accesskit_winit::Adapter>,
    /// The targets of the last tree sent, shared with the adapter's action handler.
    targets: Arc<Mutex<HashMap<u64, RequestTarget>>>,
    /// An assistive technology is connected.
    pub(crate) active: bool,
    /// Hash of the last tree sent; an unchanged hash sends nothing.
    sent_hash: Option<u64>,
    /// What the last published targets expose to act on.
    structure: Structure,
    /// The segment keyboard or assistive-technology focus is on.
    pub(crate) focus: Option<StatusSegmentId>,
    /// The tab that was active when focus moved to the strip; a tab change ends focus.
    focus_tab: Option<TabId>,
    /// The ring drawn for `focus`, with its `strong_focus` setting.
    ring: Option<(StatusSegmentId, bool)>,
    /// The announcement node's text, kept between announcements.
    announcement: Option<Announcement>,
    /// Transitions waiting for their segment's interval, at most one per segment.
    queue: Vec<Pending>,
    /// When and what each segment last announced, one entry per segment.
    announced: HashMap<StatusSegmentId, (Instant, String)>,
    /// When the last announcement was delivered.
    last_delivery: Option<Instant>,
    /// The health severity last seen.
    health: Severity,
    /// Health's spoken name last seen; `None` while nothing is wrong.
    health_text: Option<String>,
    /// Bumped by each resize or scale change the adapter is handed.
    resize_epoch: u64,
    /// The tab bar's new-mark count when last read; a higher count means a tab got a new mark.
    seen_mark: u64,
    /// A connection reached a warm spare: its first flush as a terminal window takes the baseline.
    baseline_pending: bool,
    /// The ledger charge of this model, under the window's owner.
    charges: HashMap<ResourceClass, sonicterm_resource::CommittedReservation>,
    /// Every update this window sent, for tests that read the built tree.
    #[cfg(test)]
    pub(crate) sent: Vec<TreeUpdate>,
    /// Tests: resolve this node, as the action handler would, between building a tree and sending it.
    #[cfg(test)]
    pub(crate) resolve_before_send: Option<u64>,
    /// The target resolved there, as a request made at that moment would carry.
    #[cfg(test)]
    pub(crate) resolved_before_send: Option<RequestTarget>,
}

impl WindowAccessibility {
    /// Install a dormant adapter on `window`, which must not have been shown yet. Without an
    /// event-loop proxy there is no adapter, and the window still keeps strip focus.
    pub(crate) fn install(
        event_loop: &ActiveEventLoop,
        window: &Window,
        proxy: Option<&EventLoopProxy<UserEvent>>,
    ) -> Self {
        let targets = Arc::new(Mutex::new(HashMap::new()));
        let adapter = proxy.map(|proxy| {
            let handlers = AdapterHandlers {
                window_id: window.id(),
                proxy: proxy.clone(),
                targets: Arc::clone(&targets),
            };
            accesskit_winit::Adapter::with_direct_handlers(
                event_loop,
                window,
                handlers.clone(),
                handlers.clone(),
                handlers,
            )
        });
        tracing::info!(
            window_id = ?window.id(),
            adapter = adapter.is_some(),
            "accessibility adapter installed, dormant"
        );
        Self { adapter, targets, ..Self::default() }
    }

    /// Whether a platform adapter is installed.
    #[cfg(test)]
    pub(crate) fn has_adapter(&self) -> bool {
        self.adapter.is_some()
    }

    /// Hand a window event to the adapter before the App handles it, as accesskit requires.
    pub(crate) fn process_event(&mut self, window: &Window, event: &WindowEvent) {
        if let Some(adapter) = self.adapter.as_mut() {
            adapter.process_event(window, event);
        }
    }

    /// Send a full tree update; the adapter applies it only while it is active.
    fn send(&mut self, update: TreeUpdate) {
        #[cfg(test)]
        self.sent.push(update.clone());
        if let Some(adapter) = self.adapter.as_mut() {
            adapter.update_if_active(|| update);
        }
    }

    /// The target `node` resolves to now, as the adapter's action handler would resolve it.
    #[cfg(test)]
    pub(crate) fn resolve(&self, node: u64) -> Option<RequestTarget> {
        resolve_target(&self.targets, node)
    }

    /// Publish the targets of the exposed structure for the action handler to resolve against.
    fn publish_targets(&mut self) {
        let table: HashMap<u64, RequestTarget> = self
            .structure
            .segments
            .iter()
            .map(|exposed| (segment_node(exposed.id).0, target_of(&self.structure, exposed)))
            .collect();
        if let Ok(mut targets) = self.targets.lock() {
            *targets = table;
        }
        // A poisoned table keeps its old targets; every request against them is then dropped.
    }

    /// Queue `text` as `id`'s latest transition, replacing one still waiting, unless `announce`
    /// does not list `id` or `text` is what `id` last said.
    fn queue_transition(
        &mut self,
        settings: &ResolvedAccessibility,
        id: StatusSegmentId,
        text: String,
        assertive: bool,
        now: Instant,
    ) {
        if !settings.announce.contains(&id) {
            // When: `announce` does not list `id`, the segment never announces.
            return;
        }
        self.queue.retain(|pending| pending.id != id);
        if self.announced.get(&id).is_some_and(|(_, said)| *said == text) {
            // When: the latest transition says what `announced` last said, nothing new is queued.
            return;
        }
        self.queue.push(Pending { id, text, assertive, queued_at: now });
    }

    /// When `pending` may be delivered: after its segment's interval and the announcement spacing.
    fn ready_at(&self, pending: &Pending, interval: Duration) -> Instant {
        let segment = self
            .announced
            .get(&pending.id)
            .map_or(pending.queued_at, |(at, _)| (*at + interval).max(pending.queued_at));
        self.last_delivery.map_or(segment, |last| segment.max(last + ANNOUNCE_SPACING))
    }

    /// Put the most urgent ready transition on the announcement node, assertive before polite and
    /// then oldest first. One per tree update, so a later one never overwrites it unheard.
    fn deliver_due(&mut self, settings: &ResolvedAccessibility, now: Instant) {
        let interval = Duration::from_millis(settings.announce_min_interval_ms);
        let next = self
            .queue
            .iter()
            .enumerate()
            .filter(|(_, pending)| {
                settings.announce.contains(&pending.id) && self.ready_at(pending, interval) <= now
            })
            .min_by_key(|(_, pending)| (!pending.assertive, pending.queued_at))
            .map(|(index, _)| index);
        let Some(index) = next else {
            // When: `next` is None, no queued transition is ready and the node keeps its text.
            return;
        };
        let pending = self.queue.remove(index);
        self.announced.insert(pending.id, (now, pending.text.clone()));
        self.last_delivery = Some(now);
        self.announcement = Some(Announcement { text: pending.text, assertive: pending.assertive });
    }

    /// When the next queued transition becomes ready.
    fn next_wake(&self, settings: &ResolvedAccessibility) -> Option<Instant> {
        let interval = Duration::from_millis(settings.announce_min_interval_ms);
        self.queue
            .iter()
            .filter(|pending| settings.announce.contains(&pending.id))
            .map(|pending| self.ready_at(pending, interval))
            .min()
    }

    /// Bytes this model retains as allocated storage: hash tables through the shared table estimate
    /// (buckets and control bytes), vectors and strings by capacity.
    fn retained_bytes(&self) -> usize {
        // A poisoned table is counted as empty; its old allocation is unreachable either way.
        let targets = self.targets.lock().map_or(0, |targets| {
            sonicterm_types::retained_hash_table_bytes::<u64, RequestTarget>(targets.capacity())
        });
        sonicterm_types::retained_hash_table_bytes::<StatusSegmentId, (Instant, String)>(
            self.announced.capacity(),
        ) + self.announced.values().map(|(_, said)| said.capacity()).sum::<usize>()
            + self.queue.capacity() * std::mem::size_of::<Pending>()
            + self.queue.iter().map(|pending| pending.text.capacity()).sum::<usize>()
            + self.structure.segments.capacity() * std::mem::size_of::<Exposed>()
            + self.announcement.as_ref().map_or(0, |said| said.text.capacity())
            + self.health_text.as_ref().map_or(0, String::capacity)
            + targets
    }

    /// Items this model retains: one per exposed segment, queued transition and announced text.
    fn retained_items(&self) -> usize {
        self.structure.segments.len()
            + self.queue.len()
            + self.announced.len()
            + usize::from(self.announcement.is_some())
    }
}

/// Everything a window's tree is built from.
pub(crate) struct TreeInputs<'strip> {
    /// The window's native title, the root's name.
    pub(crate) title: String,
    /// "Status line", localized.
    pub(crate) strip_label: String,
    /// The announcement node's description, localized.
    pub(crate) announcements_label: String,
    /// The strip's band and segments; `None` while the strip is off or not exposed.
    pub(crate) strip: Option<(Rect, &'strip AccessibleStrip)>,
    /// Where the announcement node sits: the band, or the window's bottom edge.
    pub(crate) announcement_bounds: Rect,
    /// The announcement node's text.
    pub(crate) announcement: Option<Announcement>,
    /// The segment with focus.
    pub(crate) focus: Option<StatusSegmentId>,
}

impl App {
    /// Install a dormant adapter on `window` before it is first shown, kept under its id.
    pub(super) fn install_accessibility(&mut self, event_loop: &ActiveEventLoop, window: &Window) {
        let state =
            WindowAccessibility::install(event_loop, window, self.event_loop_proxy.as_ref());
        self.accessibility.insert(window.id(), state);
    }

    /// Hand `event` to `window_id`'s adapter before the App handles it, a warm spare's included.
    pub(super) fn forward_accessibility_event(&mut self, window_id: WindowId, event: &WindowEvent) {
        let Self { windows, warm_window_pool, accessibility, .. } = self;
        let Some(state) = accessibility.get_mut(&window_id) else {
            // When: `accessibility` has no entry for `window_id`, the window has no adapter.
            return;
        };
        if matches!(event, WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }) {
            // A resize or scale change replaces the tree's bounds before any frame commits a layout.
            state.resize_epoch = state.resize_epoch.wrapping_add(1);
        }
        let native =
            windows.get(&window_id).and_then(|window| window.window.as_deref()).or_else(|| {
                warm_window_pool
                    .iter()
                    .find(|warm| warm.window.id() == window_id)
                    .map(|warm| &*warm.window)
            });
        if let Some(native) = native {
            state.process_event(native, event);
        }
    }

    /// The presented segments of `window_id`, left to right.
    fn committed_ids(&self, window_id: WindowId) -> Vec<StatusSegmentId> {
        let mut segments: Vec<(f32, StatusSegmentId)> = self
            .windows
            .get(&window_id)
            .and_then(|window| window.status_strip.as_ref())
            .map(|strip| strip.segments.iter().map(|segment| (segment.x, segment.id)).collect())
            .unwrap_or_default();
        segments.sort_by(|left, right| left.0.total_cmp(&right.0));
        segments.into_iter().map(|(_, id)| id).collect()
    }

    /// What `window_id`'s tree exposes now: the last presented strip, less sensitive segments while
    /// they are hidden, nothing while the strip is off or not exposed, and the focused pane.
    fn exposed_structure(&self, window_id: WindowId) -> Structure {
        let Some(window) = self.windows.get(&window_id) else {
            // When: `windows` has no `window_id`, nothing is exposed.
            return Structure::default();
        };
        let pane = window.tab_states.get(window.tabs.active_index()).map(|tab| tab.active_pane);
        let epoch = self.accessibility.get(&window_id).map_or(0, |state| state.resize_epoch);
        let settings = self.status_generation();
        let shown = self.status_line_shown() && self.status_accessibility().expose;
        let hide_sensitive = self.status_hide_sensitive();
        let Some(strip) = window.status_strip.as_ref().filter(|_| shown) else {
            // When: no strip was presented or `shown` is false, the tree exposes no segment.
            return Structure { pane, epoch, settings, ..Structure::default() };
        };
        let mut segments: Vec<Exposed> = strip
            .segments
            .iter()
            .filter(|segment| !hide_sensitive || segment.id.meta().sensitivity == Sensitivity::None)
            .map(|segment| Exposed {
                id: segment.id,
                left_px: segment.x,
                width: segment.width,
                action: segment.click.as_ref().map(action_fingerprint),
            })
            .collect();
        segments.sort_by(|left, right| left.left_px.total_cmp(&right.left_px));
        Structure { segments, band: Some((strip.top, strip.height)), pane, epoch, settings }
    }

    /// The one accessibility step of a dispatch, run from `about_to_wait` after the redraws: drop
    /// entries whose window and spare are gone, then bring each connected or strip-focused window
    /// up to date. Returns when the next queued announcement becomes ready.
    pub(super) fn flush_accessibility(&mut self, now: Instant) -> Option<Instant> {
        let Self { windows, warm_window_pool, accessibility, .. } = self;
        accessibility.retain(|window_id, _| {
            windows.contains_key(window_id)
                || warm_window_pool.iter().any(|warm| warm.window.id() == *window_id)
        });
        let watched: Vec<WindowId> = accessibility
            .iter()
            .filter(|(window_id, state)| {
                (state.active || state.focus.is_some()) && windows.contains_key(window_id)
            })
            .map(|(window_id, _)| *window_id)
            .collect();
        let mut wake: Option<Instant> = None;
        for window_id in watched {
            if let Some(next) = self.flush_window_accessibility(window_id, now) {
                wake = Some(wake.map_or(next, |current| current.min(next)));
            }
        }
        wake
    }

    /// Send About window `window_id`'s tree while a reader is connected and its rows changed.
    fn flush_about_accessibility(&mut self, window_id: WindowId) {
        let title = self
            .windows
            .get(&window_id)
            .map(|window| window.custom_window_name.clone())
            .unwrap_or_default();
        let Some((rows, copy)) = self
            .about
            .view
            .as_ref()
            .map(|view| (view.accessible.clone(), view.button_label.clone()))
        else {
            // When: view is None, About shows nothing yet, so there is no tree to send.
            return;
        };
        let Some(state) = self.accessibility.get_mut(&window_id).filter(|state| state.active)
        else {
            // When: no reader is connected (`active` is false), no tree is built.
            return;
        };
        let mut hasher = DefaultHasher::new();
        (&title, &rows, &copy).hash(&mut hasher);
        let hash = hasher.finish();
        if state.sent_hash == Some(hash) {
            // When: sent_hash matches, the reader already has these rows.
            return;
        }
        state.sent_hash = Some(hash);
        state.send(build_about_tree(&title, &rows, &copy));
    }

    /// Answer a request to About window `window_id`: a connection sends its tree on the next flush,
    /// and a press on Copy copies; its rows are text, so nothing else runs.
    fn handle_about_accessibility_request(
        &mut self,
        window_id: WindowId,
        request: AccessibilityRequest,
    ) {
        match request {
            AccessibilityRequest::InitialTree | AccessibilityRequest::Deactivated => {
                // A reader connected or left: the next flush sends a full tree, or none.
                if let Some(state) = self.accessibility.get_mut(&window_id) {
                    state.active = request == AccessibilityRequest::InitialTree;
                    state.sent_hash = None;
                }
            }
            AccessibilityRequest::Click { node, .. } if node == ABOUT_COPY_NODE.0 => {
                // When: the press is on the Copy node, copy as the button does.
                let _ = self.copy_about();
            }
            AccessibilityRequest::Click { .. }
            | AccessibilityRequest::Focus { .. }
            | AccessibilityRequest::Other => {
                // When: the request names a row or another action, the rows are text and run nothing.
            }
        }
    }

    /// Bring `window_id` up to date: end strip focus that no longer points at a presented segment
    /// of the same tab, and while an assistive technology is connected queue and deliver
    /// announcements from the current inputs, and send a full tree when what it reads changed.
    fn flush_window_accessibility(&mut self, window_id: WindowId, now: Instant) -> Option<Instant> {
        if self.is_about_window(window_id) {
            // When: window_id is the About window, its tree is the report rows, not a status line.
            self.flush_about_accessibility(window_id);
            return None;
        }
        let shown = self.status_line_shown();
        let settings = self.status_accessibility().clone();
        let Some(window) = self.windows.get(&window_id) else {
            // When: `windows` has no `window_id`, a warm spare waits for its promotion.
            return None;
        };
        let Some(state) = self.accessibility.get(&window_id) else {
            // When: `accessibility` has no entry, the window has no tree.
            return None;
        };
        let active_tab = window.tabs.active().map(|tab| tab.id);
        let stale = state.focus.is_some_and(|focus| {
            !shown
                || state.focus_tab != active_tab
                || !window
                    .status_strip
                    .as_ref()
                    .is_some_and(|strip| strip.segments.iter().any(|segment| segment.id == focus))
        });
        if stale {
            // The strip is off, the tab changed or the segment is gone: focus ends.
            self.end_status_focus(window_id);
        }
        if !self.accessibility.get(&window_id).is_some_and(|state| state.active) {
            // When: `active` is false, keyboard focus is kept but no tree is built.
            return None;
        }
        let structure = self.exposed_structure(window_id);
        let committed: Vec<CommittedSegment> = structure
            .segments
            .iter()
            .map(|exposed| CommittedSegment {
                id: exposed.id,
                left_px: exposed.left_px,
                width: exposed.width,
                clickable: exposed.action.is_some(),
            })
            .collect();
        let inputs = self.accessible_inputs(window_id)?;
        let mut ids: Vec<StatusSegmentId> = committed.iter().map(|segment| segment.id).collect();
        ids.extend([StatusSegmentId::Health, StatusSegmentId::Alerts]);
        // Names come from each segment's own formatter over the current inputs, listed or not.
        let view = self.accessible_view(&ids, &inputs);
        let strip = accessible_strip(&committed, &view, self.status_hide_sensitive(), &self.i18n);
        let (severity, health_text) = health_state(&view, &self.i18n);
        // Marks are detected where a tab gets them: the bar counts each new one.
        let (mark_seq, last_mark) =
            self.windows.get(&window_id).map_or((0, None), |window| window.tabs.last_mark());
        let (previous_health, previous_text, seen_mark, baseline) = self
            .accessibility
            .get(&window_id)
            .map(|state| {
                (state.health, state.health_text.clone(), state.seen_mark, state.baseline_pending)
            })
            .unwrap_or_default();
        let mut due: Vec<(StatusSegmentId, String, bool)> = Vec::new();
        if !baseline {
            // Changes since the last flush announce; a baseline only records what is there.
            if (severity, &health_text) != (previous_health, &previous_text) {
                // Any change of health's severity or text replaces its pending transition, so a stale
                // one is never delivered: the current state, or its recovery once nothing is wrong.
                let text = health_text
                    .clone()
                    .unwrap_or_else(|| self.i18n.translate("status-accessible-recovered"));
                due.push((StatusSegmentId::Health, text, severity == Severity::Error));
            }
            let phrase = last_mark
                .filter(|_| mark_seq != seen_mark)
                .and_then(|(tab, kind)| self.mark_phrase(window_id, tab, kind));
            if let Some(phrase) = phrase {
                // The newest mark since the last flush; its `alerts` entry replaces one still
                // waiting, so at most one alert is pending.
                due.push((StatusSegmentId::Alerts, phrase, false));
            }
        }
        let window = self.windows.get(&window_id)?;
        let native_size = window.window.as_ref().map(|native| native.inner_size());
        let width = native_size.map_or_else(
            || committed.iter().map(|segment| segment.left_px + segment.width).fold(0.0, f32::max),
            |size| size.width as f32,
        );
        let band = structure.band.map(|(top, height)| rect(0.0, top, width, height));
        let height = native_size.map_or(0.0, |size| size.height as f32);
        let announcement_bounds =
            band.unwrap_or_else(|| rect(0.0, (height - 1.0).max(0.0), width, 1.0));
        let title = window
            .window
            .as_ref()
            .map(|native| native.title())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| super::NATIVE_WINDOW_TITLE.to_owned());
        let strip_label = self.i18n.translate("status-accessible-strip");
        let announcements_label = self.i18n.translate("status-accessible-announcements");
        let state = self.accessibility.get_mut(&window_id)?;
        state.health = severity;
        state.health_text = health_text;
        state.seen_mark = mark_seq;
        state.baseline_pending = false;
        // A reload that stopped announcing a segment drops its pending transition and deadline.
        state.queue.retain(|pending| settings.announce.contains(&pending.id));
        for (id, text, assertive) in due {
            state.queue_transition(&settings, id, text, assertive, now);
        }
        state.deliver_due(&settings, now);
        let wake = state.next_wake(&settings);
        let restructured = state.structure != structure;
        if restructured {
            state.structure = structure;
        }
        let inputs = TreeInputs {
            title,
            strip_label,
            announcements_label,
            strip: band.map(|band| (band, &strip)),
            announcement_bounds,
            announcement: state.announcement.clone(),
            focus: state.focus,
        };
        // Names, roles, bounds and actions (the strip hash), focus and the announcement; never the
        // settings generation, so a reload that changes nothing read sends nothing.
        let mut hasher = DefaultHasher::new();
        (
            inputs.strip.map(|(_, strip)| strip.hash),
            &inputs.title,
            &inputs.strip_label,
            &inputs.announcements_label,
        )
            .hash(&mut hasher);
        for bounds in band.iter().chain([&announcement_bounds]) {
            (bounds.x0.to_bits(), bounds.y0.to_bits(), bounds.x1.to_bits(), bounds.y1.to_bits())
                .hash(&mut hasher);
        }
        (&inputs.announcement, inputs.focus).hash(&mut hasher);
        let hash = hasher.finish();
        if state.sent_hash != Some(hash) {
            let update = build_tree(&inputs);
            state.sent_hash = Some(hash);
            tracing::debug!(?window_id, nodes = update.nodes.len(), "accessibility tree sent");
            #[cfg(test)]
            if let Some(node) = state.resolve_before_send {
                state.resolved_before_send = resolve_target(&state.targets, node);
            }
            state.send(update);
            self.accessibility_tree_builds += 1;
            self.note_status_line(|counters| counters.tree_builds += 1);
        }
        if restructured {
            // The targets requests resolve against are published only after the adapter holds the
            // tree, so a request resolved against the old tree names the old targets.
            if let Some(state) = self.accessibility.get_mut(&window_id) {
                state.publish_targets();
            }
        }
        self.charge_accessibility(window_id);
        wake
    }

    /// Charge `window_id`'s retained model to the window's owner in the governor.
    fn charge_accessibility(&mut self, window_id: WindowId) {
        let Self { windows, accessibility, governor, .. } = self;
        let Some(owner) = windows
            .get(&window_id)
            .and_then(|window| window.owner.as_ref())
            .map(|guard| guard.id())
        else {
            // When: the window has no registered owner, its model stays outside the ledger.
            return;
        };
        let Some(state) = accessibility.get_mut(&window_id) else {
            // When: `accessibility` has no entry, there is no model to charge.
            return;
        };
        let amount =
            ResourceAmount { bytes: state.retained_bytes(), items: state.retained_items() };
        super::retention::charge_classes(
            governor,
            owner,
            &mut state.charges,
            [(ResourceClass::AccessibilityTree, amount)],
        );
    }
}

#[cfg(test)]
#[path = "accessibility_tests.rs"]
mod accessibility_tests;

#[path = "accessibility_tree.rs"]
mod tree;
pub(crate) use tree::{build_about_tree, build_tree, segment_node};
use tree::{health_state, rect, segment_for_node};
#[path = "accessibility_actions.rs"]
mod actions;

impl App {
    /// Act on what an assistive technology asked of `window_id`'s adapter.
    pub(super) fn handle_accessibility_request(
        &mut self,
        window_id: WindowId,
        request: AccessibilityRequest,
    ) {
        if self.is_about_window(window_id) {
            // When: window_id is the About window, its requests act on its rows and Copy.
            self.handle_about_accessibility_request(window_id, request);
            return;
        }
        match request {
            AccessibilityRequest::InitialTree => {
                // When: `request` is `InitialTree`, an assistive technology connected: what is
                // there now is the baseline, read from the tree and never announced.
                let baseline = self.accessible_baseline(window_id);
                let Some(state) = self.accessibility.get_mut(&window_id) else {
                    // When: `accessibility` has no entry, the window exposes nothing.
                    return;
                };
                tracing::info!(?window_id, "assistive technology connected");
                state.active = true;
                state.sent_hash = None;
                // A warm spare has no inputs yet: its first flush as a terminal window takes them.
                state.baseline_pending = baseline.is_none();
                if let Some((health, health_text, mark_seq)) = baseline {
                    state.health = health;
                    state.health_text = health_text;
                    state.seen_mark = mark_seq;
                }
            }
            AccessibilityRequest::Deactivated => {
                if let Some(state) = self.accessibility.get_mut(&window_id) {
                    tracing::info!(?window_id, "assistive technology disconnected");
                    state.active = false;
                    state.sent_hash = None;
                    state.structure = Structure::default();
                    state.announcement = None;
                    // The model is freed, not only emptied, so nothing stays allocated for it.
                    state.queue = Vec::new();
                    state.announced = HashMap::new();
                    state.seen_mark = 0;
                    if let Ok(mut targets) = state.targets.lock() {
                        *targets = HashMap::new();
                    }
                    // A poisoned table is dropped with the model when the window closes.
                    state.health = Severity::None;
                    state.health_text = None;
                    state.last_delivery = None;
                    state.baseline_pending = false;
                    // Nothing is retained for a disconnected reader, so its ledger charge ends.
                    state.charges.clear();
                }
            }
            AccessibilityRequest::Focus { node, target } => {
                // When: `request` is `Focus`, move focus unless the request is stale; never activate.
                let Some(id) = self.request_current(window_id, node, target, false) else {
                    // When: `request_current` is None, the target the request named is gone.
                    tracing::debug!(node, "stale accessibility focus dropped");
                    return;
                };
                self.set_status_focus(window_id, id);
            }
            AccessibilityRequest::Click { node, target } => {
                // When: `request` is `Click`, run the segment's click unless the request is stale.
                let Some(id) = self.request_current(window_id, node, target, true) else {
                    // When: `request_current` is None, a reload, resize, pane or action change, or
                    // exposure change replaced the target the click named; it is dropped.
                    tracing::debug!(node, "stale accessibility click dropped");
                    return;
                };
                self.click_status_segment(window_id, id);
            }
            AccessibilityRequest::Other => {
                // When: the request is `Other`, the strip supports no such action.
            }
        }
    }
}
