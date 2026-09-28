//! Window registry: main, child and frontmost window accessors and counts, active-pane
//! lookup, and synthetic window ids.

use super::*;

static NEXT_SYNTHETIC_CHILD_WINDOW_TAG: AtomicU64 = AtomicU64::new(1);

// Ordering: `NEXT_SYNTHETIC_CHILD_WINDOW_TAG.fetch_add` uses `Relaxed`; only the
// uniqueness of each returned tag matters, never its order against other writes.
pub(super) fn next_synthetic_child_window_id() -> WindowId {
    let tag = NEXT_SYNTHETIC_CHILD_WINDOW_TAG.fetch_add(1, Ordering::Relaxed);
    WindowId::from(u64::MAX - tag)
}

/// Stable synthetic `WindowId` addressing the main window entry without a live
/// winit window.
///
/// Lets a test seed the main entry in the window map directly. `u64::MAX` is
/// collision-free because real OS window ids never reach it. Production never
/// constructs this id: window creation uses the real `window.id()` and clears
/// any pre-existing synthetic entry first.
#[doc(hidden)]
pub fn synthetic_main_window_id() -> WindowId {
    WindowId::from(u64::MAX)
}

/// Which terminal window currently owns the OS-frontmost focus.
///
/// Keymap dispatch and the menubar drain consume this to decide where a chord
/// like Cmd+T / Cmd+W / Cmd+\\ should land.
///
/// `Other` covers any non-terminal SonicTerm window; it explicitly does NOT
/// route terminal actions and falls back to main as a safe default.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontmostKind {
    /// No window has focus, or recorded id is stale.
    None,
    /// Main terminal window is OS-frontmost.
    Main,
    /// A torn-out child terminal window is OS-frontmost. Carries the
    /// window id so the caller can index `windows`.
    Child(WindowId),
    /// A non-terminal SonicTerm window is frontmost. Terminal actions fall
    /// back to main.
    Other,
}

/// Screen-global inner origin and inner size, as the drag-merge module's
/// pure geometry struct.
///
/// A platform that refuses to report position, as some Wayland configurations
/// do, reports a `(0, 0)` origin, which leaves drag-merge best-effort there
/// rather than failing the drag outright.
pub(super) fn window_geom(window: &Window) -> crate::tab_drag::WindowGeom {
    let origin =
        window.inner_position().map(|position| (position.x, position.y)).unwrap_or_else(|_| (0, 0));
    let size = window.inner_size();
    crate::tab_drag::WindowGeom { inner_origin: origin, inner_size: (size.width, size.height) }
}

/// This window's scale factor, as the `f32` the geometry helpers expect.
#[inline]
pub(super) fn window_dpi(window: &Window) -> f32 {
    window.scale_factor() as f32
}

impl App {
    /// is the main window currently hidden / drained?
    /// `true` when the main `WindowState` is gone OR its `hidden` latch
    /// is set. The two shapes mean the same thing operationally — no
    /// visible main — so callers don't need to discriminate.
    #[doc(hidden)]
    pub fn main_is_hidden(&self) -> bool {
        match self.main() {
            Some(main) => main.hidden,
            None => true,
        }
    }

    pub(super) fn active_pane_id(&self) -> Option<u64> {
        self.main_active_pane_id()
    }

    pub(super) fn main_active_pane_id(&self) -> Option<u64> {
        let main = self.main()?;
        let tab_index = main.tabs.active_index();
        main.tab_states.get(tab_index).map(|tab| tab.active_pane)
    }

    pub(super) fn active_pane_id_for_kind(&self, kind: FrontmostKind) -> Option<u64> {
        match kind {
            FrontmostKind::Child(id) => {
                let child = self.windows.get(&id)?;
                let tab_index = child.tabs.active_index();
                child.tab_states.get(tab_index).map(|tab| tab.active_pane)
            }
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                self.main_active_pane_id()
            }
        }
    }

    pub(super) fn active_pane(&self) -> Option<&PaneState> {
        let id = self.active_pane_id()?;
        self.pane_by_id(id)
    }

    pub(super) fn pane_by_id(&self, pane_id: u64) -> Option<&PaneState> {
        self.windows.values().find_map(|window| window.panes.get(&pane_id))
    }

    pub(super) fn request_redraw_all_terminal_windows(&self) {
        for (id, window) in &self.windows {
            if Some(*id) == self.main_window_id {
                if let Some(main_window) = self.main_window() {
                    main_window.request_redraw();
                }
            } else {
                // When: `id` is not `main_window_id`, so the redraw is requested
                // on the torn-out child's own surface rather than main's.
                window.request_redraw();
            }
        }
    }

    /// Resolve a live window to its stable backend-free key; absent windows have no key.
    pub fn window_key(&self, id: WindowId) -> Option<sonicterm_types::WindowKey> {
        self.windows.contains_key(&id).then(|| self.window_keys.get(id)).flatten()
    }

    /// classify [`Self::frontmost_window`] without
    /// borrowing anything mutably. Returns:
    ///   * `FrontmostKind::None` if no sonic window has been focused yet,
    ///     focus is currently outside every sonic window, or the recorded
    ///     id no longer matches any live window (stale-id race).
    ///   * `FrontmostKind::Main` if the recorded id matches the main
    ///     window we currently own.
    ///   * `FrontmostKind::Child(id)` if the recorded id matches a live
    ///     entry in [`Self::windows`].
    ///   * `FrontmostKind::Other` for any non-terminal window — actions
    ///     should fall through to the safe
    ///     main-window default in that case.
    ///
    /// Pure read; no mutation, no logging. The keymap_dispatch arms call
    /// this first, then route to the matching mutator + redraw target.
    /// Borrow the main window's [`WindowState`] from `self.windows`, keyed by
    /// [`Self::main_window_id`]. Returns `None` before `do_resumed` has run
    /// (no main window yet) or if the entry is missing for any reason.
    ///
    /// Every reader of the main window's renderer, tabs, and panes goes
    /// through this helper or its `_mut` counterpart.
    #[doc(hidden)]
    pub fn main(&self) -> Option<&WindowState> {
        let id = self.main_window_id?;
        self.windows.get(&id)
    }

    /// Mutable counterpart of [`Self::main`].
    #[doc(hidden)]
    pub fn main_mut(&mut self) -> Option<&mut WindowState> {
        let id = self.main_window_id?;
        self.windows.get_mut(&id)
    }

    /// Borrow the main window's `Arc<Window>` from its [`WindowState`].
    /// Sole source of truth for the main window handle. Returns `None`
    /// before `do_resumed` has run.
    #[doc(hidden)]
    pub fn main_window(&self) -> Option<&Arc<Window>> {
        self.windows.get(&self.main_window_id?)?.window.as_ref()
    }

    /// borrow the main window's `GpuRenderer`
    /// from its `WindowState`. Sole source of truth for the main
    /// renderer.
    /// Returns `None` before `do_resumed` has run.
    #[doc(hidden)]
    pub fn main_renderer(&self) -> Option<&GpuRenderer> {
        self.windows.get(&self.main_window_id?)?.renderer.as_ref()
    }

    /// Mutable counterpart of [`Self::main_renderer`].
    #[doc(hidden)]
    pub fn main_renderer_mut(&mut self) -> Option<&mut GpuRenderer> {
        let id = self.main_window_id?;
        self.windows.get_mut(&id)?.renderer.as_mut()
    }

    /// borrow the main window's [`TabBar`] from
    /// its [`WindowState`]. Sole source of truth (legacy `App.tabs` was
    /// Returns `None` before `do_resumed` /
    /// `__test_synthetic_main` has populated the shadow entry.
    #[doc(hidden)]
    pub fn main_tabs(&self) -> Option<&TabBar> {
        Some(&self.windows.get(&self.main_window_id?)?.tabs)
    }

    /// Mutable counterpart of [`Self::main_tabs`].
    #[doc(hidden)]
    pub fn main_tabs_mut(&mut self) -> Option<&mut TabBar> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.tabs)
    }

    /// borrow the main window's `Vec<TabState>`
    /// from its [`WindowState`]. Sole source of truth.
    #[doc(hidden)]
    pub fn main_tab_states(&self) -> Option<&[TabState]> {
        Some(self.windows.get(&self.main_window_id?)?.tab_states.as_slice())
    }

    /// Mutable counterpart of [`Self::main_tab_states`].
    #[doc(hidden)]
    pub fn main_tab_states_mut(&mut self) -> Option<&mut Vec<TabState>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.tab_states)
    }

    /// The main window's pane map.
    ///
    /// `None` before a main window exists, which distinguishes "no window yet"
    /// from "a window holding no panes".
    pub fn main_panes(&self) -> Option<&HashMap<u64, PaneState>> {
        Some(&self.windows.get(&self.main_window_id?)?.panes)
    }

    /// Mutable counterpart of [`Self::main_panes`]. NOTE: this borrows
    /// the full main [`WindowState`] mutably via `windows.get_mut`, so
    /// callers needing panes + tabs/tab_states/renderer in one expression
    /// must instead `let ws = self.main_mut()?;` and field-disjoint
    /// split-borrow.
    #[doc(hidden)]
    pub fn main_panes_mut(&mut self) -> Option<&mut HashMap<u64, PaneState>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.panes)
    }

    /// borrow the main window's selection
    /// `Option<Selection>` from its [`WindowState`]. Sole source of
    /// truth.
    /// Returns `None` (no main window) — `Some(None)` (no selection)
    /// — `Some(Some(_))` (active selection).
    #[doc(hidden)]
    pub fn main_selection(&self) -> Option<&Option<Selection>> {
        Some(&self.windows.get(&self.main_window_id?)?.selection)
    }

    /// Mutable counterpart of [`Self::main_selection`].
    #[doc(hidden)]
    pub fn main_selection_mut(&mut self) -> Option<&mut Option<Selection>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.selection)
    }

    /// borrow the main window's
    /// `ModifiersState` from its [`WindowState`]. Returns
    /// `ModifiersState::empty()` if the main window does not yet
    /// exist (safe default — no modifiers held).
    #[doc(hidden)]
    pub fn main_modifiers(&self) -> ModifiersState {
        self.main_window_id
            .and_then(|id| self.windows.get(&id))
            .map(|main| main.modifiers)
            .unwrap_or_else(ModifiersState::empty)
    }

    /// replace the main window's selection.
    /// No-op when the main window does not yet exist.
    #[doc(hidden)]
    pub fn selection_set(&mut self, sel: Option<Selection>) {
        if let Some(main) = self.main_mut() {
            main.selection = sel;
        }
    }

    /// replace the main window's copy-mode state.
    /// No-op when the main window does not yet exist.
    #[doc(hidden)]
    pub fn copy_mode_set(&mut self, copy_mode: Option<CopyModeState>) {
        if let Some(main) = self.main_mut() {
            main.copy_mode = copy_mode;
        }
    }

    /// borrow the [`WindowState`] of whichever terminal
    /// window is OS-frontmost. Falls back to the main window when no
    /// frontmost has been recorded yet (matches the safe default in
    /// [`Self::frontmost_kind`]).
    #[doc(hidden)]
    pub fn frontmost(&self) -> Option<&WindowState> {
        let id = self.frontmost_window.or(self.main_window_id)?;
        self.windows.get(&id)
    }

    /// Mutable counterpart of [`Self::frontmost`].
    #[doc(hidden)]
    pub fn frontmost_mut(&mut self) -> Option<&mut WindowState> {
        let id = self.frontmost_window.or(self.main_window_id)?;
        self.windows.get_mut(&id)
    }

    /// Which terminal window currently holds OS focus.
    ///
    /// Keymap dispatch routes window-scoped chords by this, so a stale or
    /// unfocused id resolves to `None` rather than defaulting to main.
    #[doc(hidden)]
    pub fn frontmost_kind(&self) -> FrontmostKind {
        let Some(id) = self.frontmost_window else {
            // When: `frontmost_window` recorded nothing, so focus is unknown and
            // callers fall back to main rather than guessing a target.
            return FrontmostKind::None;
        };
        if let Some(main_window) = self.main_window() {
            // When: `main_window` exists, so its identity is checked before the
            // recorded `id` is treated as a torn-out child.
            if main_window.id() == id {
                // When: `main_window` carries the focused `id`, so the chord lands on main
                // and the child lookup below is unnecessary.
                return FrontmostKind::Main;
            }
        }
        if self.windows.contains_key(&id) {
            // When: `windows` still tracks `id` after the main check, so focus
            // sits on a live torn-out child.
            return FrontmostKind::Child(id);
        }
        // Recorded id doesn't match anything live (rare: window closed
        // between the focus event and the action dispatch). Treat as
        // "no frontmost" so callers fall back to the main-window default.
        FrontmostKind::None
    }

    /// if [`Self::frontmost_window`] is `Some(_)`
    /// but classifies as `None` (recorded id no longer matches any
    /// live window), clear it. Called by keymap_dispatch arms BEFORE
    /// falling back to main, so the next dispatch doesn't retry the
    /// dead id. Returns `true` if a stale id was cleared (purely
    /// informational; callers ignore it today).
    #[doc(hidden)]
    pub fn clear_stale_frontmost(&mut self) -> bool {
        if self.frontmost_window.is_some() && self.frontmost_kind() == FrontmostKind::None {
            // When: `frontmost_window` names a window `frontmost_kind` can no
            // longer classify, so the record outlived the window it points at.
            self.frontmost_window = None;
            return true;
        }
        false
    }

    /// How many torn-out child windows are live.
    ///
    /// The shadow main entry is excluded, so this counts only windows a user
    /// tore off rather than every entry in the map.
    #[doc(hidden)]
    pub fn child_window_count(&self) -> usize {
        self.windows.len().saturating_sub(self.shadow_main_count())
    }

    /// `1` if the shadow main entry is present in
    /// [`Self::windows`], else `0`. Used by every "count torn-out
    /// child windows" path so they keep the
    /// same number.
    #[inline]
    #[doc(hidden)]
    pub fn shadow_main_count(&self) -> usize {
        match self.main_window_id {
            Some(id) if self.windows.contains_key(&id) => 1,
            _ => 0,
        }
    }

    /// number of windows in the unified
    /// [`Self::windows`] map.
    /// Used by the regression suite to pin the rename + role tagging.
    #[doc(hidden)]
    pub fn unified_window_count(&self) -> usize {
        self.windows.len().saturating_sub(self.shadow_main_count())
    }

    /// count entries in [`Self::windows`] whose
    /// role matches the argument. Today every entry is `Terminal`;
    #[doc(hidden)]
    pub fn windows_with_role(&self, role: crate::app::WindowRole) -> usize {
        self.windows
            .iter()
            .filter(|(id, window)| window.role == role && Some(**id) != self.main_window_id)
            .count()
    }

    /// Read-only accessor used by tests and (eventually) the
    /// renderer to honor the View → Toggle Tab Bar menu item.
    #[doc(hidden)]
    pub fn tab_bar_visible(&self) -> bool {
        self.tab_bar_visible
    }
}
