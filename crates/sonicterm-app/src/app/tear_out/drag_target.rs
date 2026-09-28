//! Tab-bar drop targets for a dragged tab, and merging a dragged tab into another window.

use super::*;

impl App {
    pub(in crate::app) fn compute_child_drag_target(
        &self,
        src_id: WindowId,
        local_in_src: (f64, f64),
    ) -> Option<crate::tab_drag::DropTarget<WindowId>> {
        let src_child = self.windows.get(&src_id)?;
        let src_origin = src_child
            .window
            .as_ref()?
            .inner_position()
            .map(|position| (position.x, position.y))
            .unwrap_or_else(|_| (0, 0));
        let global = crate::tab_drag::local_to_global(src_origin, local_in_src);
        let mut candidates: Vec<(WindowId, crate::tab_drag::WindowGeom, Option<TabBarLayout>)> =
            Vec::new();
        if let Some(main) = self.main_window() {
            let geom = window_geom(main);
            let width = self.main_renderer().map(|renderer| renderer.width() as f32).unwrap_or(0.0);
            let inset =
                self.main_renderer().map(|renderer| renderer.tab_bar_y_offset()).unwrap_or(0.0);
            let bar_h = self
                .main_renderer()
                .map(|renderer| renderer.tab_bar_logical_height())
                .unwrap_or(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT);
            candidates.push((
                main.id(),
                geom,
                self.main_tabs().map(|tabs| {
                    TabBarLayout::compute_with_height(tabs, width, bar_h)
                        .with_top_offset(inset)
                        .with_visible(self.tab_bar_visible)
                }),
            ));
        }
        for (id, child) in &self.windows {
            if *id == src_id || Some(*id) == self.main_window_id {
                // When: `id` is the drag source or the window named by `main_window_id`,
                // which was already pushed above; skip so no window is a candidate twice.
                continue;
            }
            let Some(renderer) = child.renderer.as_ref() else {
                // When: the child has no `renderer`, so its width and tab-bar height are
                // unknown and no drop rect can be built; leave it out of `candidates`.
                continue;
            };
            let Some(window) = child.window.as_ref() else {
                // When: the child holds no `window`, so `window_geom` has nothing to read
                // its screen rect from; skip it rather than hit-test a placeless entry.
                continue;
            };
            let geom = window_geom(window);
            let bar_width = renderer.width() as f32;
            let layout = TabBarLayout::compute_with_height(
                &child.tabs,
                bar_width,
                renderer.tab_bar_logical_height(),
            )
            .with_top_offset(renderer.tab_bar_y_offset())
            .with_visible(renderer.tab_bar_visible());
            candidates.push((*id, geom, Some(layout)));
        }
        crate::tab_drag::find_drop_target_skipping_unrendered(global, candidates)
    }
    pub(in crate::app) fn compute_main_drag_target(
        &self,
        local_in_main: (f64, f64),
    ) -> Option<crate::tab_drag::DropTarget<WindowId>> {
        let main_window = self.main_window()?;
        let main_origin = main_window
            .inner_position()
            .map(|position| (position.x, position.y))
            .unwrap_or_else(|_| (0, 0));
        let global = crate::tab_drag::local_to_global(main_origin, local_in_main);
        let candidates = self.windows.iter().filter_map(|(id, child)| {
            if Some(*id) == self.main_window_id {
                // When: `id` is `main_window_id`, the window this drag started in; a tab
                // cannot drop onto its own source, so keep it out of `candidates`.
                return None;
            }
            let renderer = child.renderer.as_ref()?;
            let window = child.window.as_ref()?;
            let geom = window_geom(window);
            let bar_width = renderer.width() as f32;
            let layout = TabBarLayout::compute_with_height(
                &child.tabs,
                bar_width,
                renderer.tab_bar_logical_height(),
            )
            .with_top_offset(renderer.tab_bar_y_offset())
            .with_visible(renderer.tab_bar_visible());
            Some((*id, geom, Some(layout)))
        });
        crate::tab_drag::find_drop_target_skipping_unrendered(global, candidates)
    }

    pub fn try_cross_window_merge(&mut self, index: usize) -> bool {
        let main_id = self.main_window_id;
        let Some(target) = self
            .main()
            .and_then(|window| window.drag_target)
            .filter(|target| Some(target.window) != main_id)
        else {
            // When: no `drag_target` names a window other than `main_id`, so there is no
            // destination to merge into; return false so the caller tears out instead.
            return false;
        };
        if let Some(window) = self.main_mut() {
            window.drag_target = None;
            window.pressed_tab = None;
            window.mouse_down = false;
        }
        self.merge_main_into_child(index, target)
    }
}
