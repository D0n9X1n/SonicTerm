//! Renderer, allocator and graphics snapshot formatting and aggregation.

use super::*;

/// Select one renderer in main/visible/warm priority and read its shared device once.
///
/// Visible candidates use their lowest label so `HashMap` iteration cannot change attribution;
/// `FnOnce` makes a second allocator query impossible in this call.
pub(super) fn read_authoritative_allocator<'a, T>(
    main: Option<(&'a str, &'a T)>,
    visible: &[(&'a str, &'a T)],
    warm: &[(&'a str, &'a T)],
    read: impl FnOnce(&T) -> Option<AllocatorSnapshot>,
) -> Option<AllocatorReading> {
    // When: main Some selects its device; visible min_by_key and warm first provide ordered fallbacks; empty inputs return None.
    let (source, label, renderer) = if let Some((label, renderer)) = main {
        (AllocatorSource::MainWindow, label, renderer)
    } else if let Some((label, renderer)) = visible.iter().min_by_key(|(label, _)| *label).copied()
    {
        (AllocatorSource::VisibleWindow, label, renderer)
    } else if let Some((label, renderer)) = warm.first().copied() {
        (AllocatorSource::WarmPool, label, renderer)
    } else {
        return None;
    };

    Some(AllocatorReading { source, label: label.to_string(), snapshot: read(renderer) })
}

impl RendererSummary {
    /// Render as one field of the composed renderer breakdown.
    ///
    /// Deliberately terse and positional: this string appears once per
    /// renderer inside a single log field, so a verbose form would push the
    /// interesting totals off the end of a line an operator is skimming.
    pub(super) fn render(&self) -> String {
        let total = self.total();
        format!(
            "{}[{}] glyph={}/{} image={}/{} row_glyph={}/{} row_quad={}/{} software={}/{} vertex={}/{} row_ink={}/{} frame_scratch={}/{} chrome_cache={}/{} total={}/{}",
            self.role,
            self.label,
            self.glyph_atlas.bytes,
            self.glyph_atlas.items,
            self.image_atlas.bytes,
            self.image_atlas.items,
            self.row_glyph_cache.bytes,
            self.row_glyph_cache.items,
            self.row_quad_cache.bytes,
            self.row_quad_cache.items,
            self.software_frame.bytes,
            self.software_frame.items,
            self.vertex_scratch.bytes,
            self.vertex_scratch.items,
            self.row_ink.bytes,
            self.row_ink.items,
            self.frame_scratch.bytes,
            self.frame_scratch.items,
            self.chrome_cache.bytes,
            self.chrome_cache.items,
            total.bytes,
            total.items,
        ) + &format!(
            " glyph_atlas_dim={} glyph_atlas_packed_pixels={} glyph_atlas_growths={} \
             glyph_atlas_evictions={} glyph_atlas_fit={} glyph_atlas_max_tile={}x{}",
            self.glyph_atlas_facts.dim,
            self.glyph_atlas_facts.packed_pixels,
            self.glyph_atlas_facts.growths,
            self.glyph_atlas_facts.evictions,
            self.glyph_atlas_facts.fit,
            self.glyph_atlas_facts.max_tile[0],
            self.glyph_atlas_facts.max_tile[1],
        ) + &format!(
            // Renderer-local names: the checkpoint line also carries a top-level `trimmed`, and a
            // reader scanning for `name=` must never find this one first.
            " renderer_trimmed={} renderer_gpu_released_requested_bytes={}",
            self.trimmed, self.gpu_released_requested_bytes
        )
    }
}

impl MemorySnapshot {
    /// The per-renderer breakdown, as one field.
    ///
    /// `none` rather than an empty string when no renderer exists, so the
    /// field is never blank — a blank field reads as a bug in the logger
    /// rather than as a session with no window yet.
    #[must_use]
    pub fn render_renderers(&self) -> String {
        if self.renderers.is_empty() {
            // When: `renderers` is empty, emit an explicit sentinel instead of an ambiguous blank field.
            return "none".to_string();
        }
        let mut renderers = self.renderers.iter().collect::<Vec<_>>();
        renderers.sort_by(|left, right| {
            (left.role, left.label.as_str()).cmp(&(right.role, right.label.as_str()))
        });
        renderers.into_iter().map(RendererSummary::render).collect::<Vec<_>>().join("; ")
    }

    /// Classify allocator telemetry as measured, backend-unsupported, or renderer-absent.
    pub(super) fn allocator_state(&self) -> AllocatorState {
        match self.allocator.as_ref().and_then(|reading| reading.snapshot) {
            Some(_) => AllocatorState::Measured,
            None if self.allocator.is_some() => AllocatorState::Unsupported,
            None => AllocatorState::NoRenderer,
        }
    }

    /// Project one measured allocator scalar while preserving both absence states.
    pub(super) fn allocator_metric(
        &self,
        read: impl FnOnce(AllocatorSnapshot) -> u64,
    ) -> AllocatorMetric {
        match self.allocator.as_ref().and_then(|reading| reading.snapshot) {
            Some(snapshot) => AllocatorMetric::Measured(read(snapshot)),
            None if self.allocator.is_some() => AllocatorMetric::Unsupported,
            None => AllocatorMetric::NoRenderer,
        }
    }

    /// Return the selected renderer-class label, or `none` before any renderer exists.
    pub(super) fn allocator_source(&self) -> &'static str {
        self.allocator.as_ref().map_or("none", |reading| reading.source.as_str())
    }

    /// Return the selected renderer identifier, or `none` when no query was attempted.
    pub(super) fn allocator_label(&self) -> &str {
        self.allocator.as_ref().map_or("none", |reading| reading.label.as_str())
    }
}

impl RendererGraphics {
    /// Render as one entry of the composed `graphics_renderers` field: `bytes/resources` per class.
    pub(super) fn render(&self) -> String {
        let mut text = format!("{}[{}]", self.role, self.label);
        for class in GraphicsClass::ALL {
            let tally = self.ledger.class(class);
            text.push_str(&format!(
                " {}={}/{}",
                class.as_str(),
                tally.live_bytes,
                tally.live_resources
            ));
        }
        text + &format!(
            " requested_total={}/{} surface_count={}",
            self.ledger.live_bytes(),
            self.ledger.live_resources(),
            self.ledger.surface_count_kind()
        )
    }
}

impl GraphicsReading {
    /// Requested GPU bytes every renderer holds now.
    #[must_use]
    pub fn requested_live_bytes(&self) -> u64 {
        self.renderers
            .iter()
            .fold(0_u64, |total, renderer| total.saturating_add(renderer.ledger.live_bytes()))
    }

    /// GPU resources every renderer holds now.
    #[must_use]
    pub fn requested_live_resources(&self) -> u64 {
        self.renderers
            .iter()
            .fold(0_u64, |total, renderer| total.saturating_add(renderer.ledger.live_resources()))
    }

    /// Every renderer's tallies summed per class: created and released totals across live renderers.
    ///
    /// A dropped renderer's totals are not here; its drop logs them once as
    /// `renderer graphics teardown`.
    #[must_use]
    pub fn summed(&self) -> GraphicsLedgerSnapshot {
        let mut total = GraphicsLedgerSnapshot::default();
        for renderer in &self.renderers {
            for (sum, tally) in total.classes.iter_mut().zip(renderer.ledger.classes) {
                sum.live_bytes = sum.live_bytes.saturating_add(tally.live_bytes);
                sum.live_resources = sum.live_resources.saturating_add(tally.live_resources);
                sum.created_bytes = sum.created_bytes.saturating_add(tally.created_bytes);
                sum.created_resources =
                    sum.created_resources.saturating_add(tally.created_resources);
                sum.released_bytes = sum.released_bytes.saturating_add(tally.released_bytes);
                sum.released_resources =
                    sum.released_resources.saturating_add(tally.released_resources);
            }
            total.surface_estimated |= renderer.ledger.surface_estimated;
        }
        total
    }

    /// Every renderer's ledger as one field, sorted like `renderers`; `none` when there is none.
    #[must_use]
    pub fn render_renderers(&self) -> String {
        if self.renderers.is_empty() {
            // When: `renderers` is empty, emit an explicit sentinel instead of a blank field.
            return "none".to_string();
        }
        let mut renderers = self.renderers.iter().collect::<Vec<_>>();
        renderers.sort_by(|left, right| {
            (left.role, left.label.as_str()).cmp(&(right.role, right.label.as_str()))
        });
        renderers.into_iter().map(RendererGraphics::render).collect::<Vec<_>>().join("; ")
    }
}

#[cfg(test)]
#[path = "memory_snapshot_renderer_tests.rs"]
mod memory_snapshot_renderer_tests;
