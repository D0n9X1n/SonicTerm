//! Extracted from `app/mod.rs` in refactor PR 8b (expose-then-extract).
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use sonic_core::{
    config::Config,
    grid::Grid,
    keymap::{Action, Direction, Keymap, ScrollAction},
    pty::PtyHandle,
    theme::Theme,
    vt::{Parser, VtEvent},
};
use sonic_shared::render::GpuRenderer;
use sonic_ui::pane::PaneTree;
use sonic_ui::selection::Selection;
use sonic_ui::tabbar_view::{TabBarLayout, TabHit};
use sonic_ui::tabs::{Tab, TabBar};
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    to_logical_pos, with_integrated_titlebar, wrap_paste, App, PaneState, TabState, UserEvent,
    WindowState,
};
use crate::app::integrated_titlebar_inset;

impl App {
    pub fn detach_tab_state(
        &mut self,
        index: usize,
    ) -> Option<(Tab, TabState, HashMap<u64, PaneState>)> {
        if index >= self.tab_states.len() || index >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.tabs().get(index).cloned()?;
        let state = self.tab_states.remove(index);
        let mut panes: HashMap<u64, PaneState> = HashMap::new();
        for id in state.tree.leaves() {
            if let Some(p) = self.panes.remove(&id) {
                panes.insert(id, p);
            }
        }
        self.tabs.close(tab.id);
        Some((tab, state, panes))
    }
    pub fn attach_tab_state(
        &mut self,
        index: usize,
        tab: Tab,
        state: TabState,
        panes: HashMap<u64, PaneState>,
    ) {
        let (cols, rows) = self.renderer.as_ref().map(|r| r.cells()).unwrap_or((80, 24));
        for (id, pane) in panes {
            pane.parser.lock().grid_mut().resize(cols, rows);
            if let Some(pty) = pane.pty.as_ref() {
                (pty.resize)(cols, rows);
            }
            *pane.redraw_target.lock() = self.main_window().cloned();
            self.panes.insert(id, pane);
        }
        let idx = index.min(self.tabs.len());
        self.tabs.insert(idx, tab);
        self.tab_states.insert(idx, state);
    }
    pub fn detach_from_child(
        &mut self,
        src_id: WindowId,
        index: usize,
    ) -> Option<(Tab, TabState, HashMap<u64, PaneState>)> {
        let child = self.windows.get_mut(&src_id)?;
        if index >= child.tabs.len() || index >= child.tab_states.len() {
            return None;
        }
        let tab = child.tabs.tabs().get(index).cloned()?;
        let state = child.tab_states.remove(index);
        let mut panes: HashMap<u64, PaneState> = HashMap::new();
        for id in state.tree.leaves() {
            if let Some(p) = child.panes.remove(&id) {
                panes.insert(id, p);
            }
        }
        child.tabs.close(tab.id);
        Some((tab, state, panes))
    }
    pub fn attach_to_child(
        &mut self,
        dst_id: WindowId,
        index: usize,
        tab: Tab,
        state: TabState,
        panes: HashMap<u64, PaneState>,
    ) -> bool {
        let Some(child) = self.windows.get_mut(&dst_id) else { return false };
        let Some(renderer) = child.renderer.as_ref() else { return false };
        let (cols, rows) = renderer.cells();
        for (id, pane) in panes {
            pane.parser.lock().grid_mut().resize(cols, rows);
            if let Some(pty) = pane.pty.as_ref() {
                (pty.resize)(cols, rows);
            }
            *pane.redraw_target.lock() = Some(child.window.clone());
            child.panes.insert(id, pane);
        }
        let idx = index.min(child.tabs.len());
        child.tabs.insert(idx, tab);
        child.tab_states.insert(idx, state);
        child.window.request_redraw();
        true
    }
}
