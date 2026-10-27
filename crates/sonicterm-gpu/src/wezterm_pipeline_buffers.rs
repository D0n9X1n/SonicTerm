//! Presentation buffer allocation, growth, shrink and ledger accounting.

use super::*;

/// Create the shader-uniform buffer without initial contents.
///
/// `draw_frame` writes the uniform before every draw, so no initial contents
/// are needed. A plain `create_buffer` also keeps an invalid descriptor a
/// contained wgpu error: `create_buffer_init` maps the new buffer and panics
/// through `expect` when that buffer is invalid.
pub(super) fn create_uniform_buffer(
    device: &wgpu::Device,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sonic-wezterm-uniform"),
        size: std::mem::size_of::<ShaderUniform>() as u64,
        usage,
        mapped_at_creation: false,
    })
}

/// Create an unmapped index buffer holding `index_capacity` indices.
///
/// Every index buffer goes through here, at creation and at growth. A plain
/// `create_buffer` keeps an invalid descriptor a contained wgpu error, where
/// `create_buffer_init` would map the new buffer and panic on an invalid one.
pub(super) fn create_index_buffer(
    device: &wgpu::Device,
    index_capacity: u64,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sonic-wezterm-present-indices"),
        size: index_capacity * std::mem::size_of::<u32>() as u64,
        usage,
        mapped_at_creation: false,
    })
}

/// Shrink `scratch` after a frame that used `used_vertices` of it.
///
/// A capacity over four times the frame's use and over 1 MiB is cut to twice that use,
/// so one large frame does not pin its peak for the window's life. A frame that used no
/// vertices therefore releases a scratch over 1 MiB entirely; a smaller one is kept.
/// Contents are kept.
pub(crate) fn release_scratch_excess(scratch: &mut Vec<Vertex>, used_vertices: usize) {
    release_excess(scratch, used_vertices);
}

/// Shrink any reused `scratch` after a pass that used `used` of its elements, by the vertex
/// scratch's rule: a capacity over four times the use and over 1 MiB is cut to twice the use.
/// Contents are kept.
pub(crate) fn release_excess<T>(scratch: &mut Vec<T>, used: usize) {
    let capacity_bytes = scratch.capacity().saturating_mul(std::mem::size_of::<T>());
    if scratch.capacity() > used.saturating_mul(4) && capacity_bytes > SCRATCH_RELEASE_FLOOR_BYTES {
        // The scratch is both oversized for this pass and large in absolute terms.
        scratch.shrink_to(used.saturating_mul(2));
    }
}

impl ShrinkWindow {
    /// Record one draw of `used_quads` against a buffer holding `capacity_quads`; at the end of a
    /// window, return the quad capacity to shrink to, `max(initial_quads, next_pow2(2 × peak))`,
    /// when the buffer is over four times the window's peak and over `initial_quads`.
    pub(crate) fn observe(
        &mut self,
        used_quads: u64,
        capacity_quads: u64,
        initial_quads: u64,
    ) -> Option<u64> {
        self.calls += 1;
        self.peak_quads = self.peak_quads.max(used_quads);
        if self.calls < SHRINK_WINDOW_CALLS {
            // When: `calls` has not reached the window length, keep observing.
            return None;
        }
        let peak = self.peak_quads;
        *self = Self::default();
        let oversized = capacity_quads > peak.saturating_mul(4) && capacity_quads > initial_quads;
        oversized.then(|| initial_quads.max(peak.saturating_mul(2).max(1).next_power_of_two()))
    }
}

impl WeztermPipeline {
    /// Report this pipeline's three buffers to `ledger` now, and every later recreation or growth.
    pub(crate) fn attach_graphics_ledger(
        &mut self,
        ledger: crate::graphics_ledger::GraphicsLedgerHandle,
    ) {
        ledger.replace(
            crate::graphics_ledger::GraphicsSlot::PresentUniform,
            std::mem::size_of::<ShaderUniform>() as u64,
            1,
        );
        self.graphics_ledger = Some(ledger);
        self.record_vertex_request();
        self.record_index_request();
    }
}

impl WeztermPipeline {
    /// Record the vertex buffer's requested bytes in the attached ledger, if any.
    pub(super) fn record_vertex_request(&self) {
        if let Some(ledger) = &self.graphics_ledger {
            ledger.replace(
                crate::graphics_ledger::GraphicsSlot::PresentVertices,
                self.vertex_capacity * std::mem::size_of::<Vertex>() as u64,
                1,
            );
        }
    }
}

impl WeztermPipeline {
    /// Record the index buffer's requested bytes in the attached ledger, if any.
    pub(super) fn record_index_request(&self) {
        if let Some(ledger) = &self.graphics_ledger {
            ledger.replace(
                crate::graphics_ledger::GraphicsSlot::PresentIndices,
                self.index_capacity * std::mem::size_of::<u32>() as u64,
                1,
            );
        }
    }
}

impl WeztermPipeline {
    /// Bytes both present buffers request from the device.
    pub(crate) fn present_buffer_bytes(&self) -> u64 {
        self.vertex_capacity * std::mem::size_of::<Vertex>() as u64
            + self.index_capacity * std::mem::size_of::<u32>() as u64
    }
}

impl WeztermPipeline {
    /// Recreate both present buffers to hold exactly `quads` quads; the new index buffer holds
    /// no pattern yet, so the next draw writes it.
    pub(super) fn recreate_buffers(&mut self, device: &wgpu::Device, quads: u64) {
        let vertex_capacity = quads * VERTICES_PER_QUAD as u64;
        let index_capacity = quads * INDICES_PER_QUAD as u64;
        self.vertex_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sonic-wezterm-present-vertices"),
            size: vertex_capacity * std::mem::size_of::<Vertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.index_buf = create_index_buffer(device, index_capacity, INDEX_BUFFER_USAGE);
        self.vertex_capacity = vertex_capacity;
        self.index_capacity = index_capacity;
        self.index_pattern_quads = 0;
        self.record_vertex_request();
        self.record_index_request();
    }
}

impl WeztermPipeline {
    /// Return both present buffers to their initial size and empty the vertex scratch, for a
    /// covered window's trim; returns the device bytes the buffers no longer request.
    pub(crate) fn reset_to_initial(&mut self, device: &wgpu::Device) -> u64 {
        let before = self.present_buffer_bytes();
        self.recreate_buffers(device, self.initial_quads);
        self.shrink_window = ShrinkWindow::default();
        self.vertex_scratch = Vec::new();
        before.saturating_sub(self.present_buffer_bytes())
    }
}

impl WeztermPipeline {
    /// CPU storage held by the reusable vertex scratch: its capacity in bytes, one item.
    pub(crate) fn vertex_scratch_retained(&self) -> ResourceAmount {
        let capacity = self.vertex_scratch.capacity();
        ResourceAmount {
            bytes: capacity.saturating_mul(std::mem::size_of::<Vertex>()),
            items: usize::from(capacity > 0),
        }
    }
}

impl WeztermPipeline {
    /// Write the index pattern for the buffer's whole quad capacity when the buffer does
    /// not hold it yet (the first frame and the first after growth); return the bytes written.
    ///
    /// Runs inside the renderer's device-error boundary like every other frame write.
    pub(super) fn upload_index_pattern(&mut self, queue: &wgpu::Queue) -> usize {
        let quad_capacity = self.index_capacity / INDICES_PER_QUAD as u64;
        if self.index_pattern_quads >= quad_capacity {
            // When: `index_pattern_quads` already covers `quad_capacity`; every draw range is a prefix.
            return 0;
        }
        let pattern = build_indices(quad_capacity as usize);
        let pattern_bytes: &[u8] = bytemuck::cast_slice(&pattern);
        queue.write_buffer(&self.index_buf, 0, pattern_bytes);
        self.index_pattern_quads = quad_capacity;
        pattern_bytes.len()
    }
}

impl WeztermPipeline {
    /// Feed one draw of `used_quads` to the shrink window and recreate both buffers at the size it
    /// asks for. The target is at least twice the window's peak, so it holds this draw too.
    pub(super) fn shrink_if_oversized(&mut self, device: &wgpu::Device, used_quads: u64) {
        let capacity_quads = (self.vertex_capacity / VERTICES_PER_QUAD as u64)
            .min(self.index_capacity / INDICES_PER_QUAD as u64);
        if let Some(quads) =
            self.shrink_window.observe(used_quads, capacity_quads, self.initial_quads)
        {
            // The window closed with the buffers oversized, so both are recreated smaller.
            self.recreate_buffers(device, quads);
        }
    }
}

impl WeztermPipeline {
    /// Grow the vertex buffer to hold `vertices` vertices and the index buffer to hold
    /// `indices` indices, doubling each; a grown index buffer holds no pattern yet.
    pub(super) fn ensure_capacity(&mut self, device: &wgpu::Device, vertices: u64, indices: u64) {
        if vertices > self.vertex_capacity {
            let mut cap = self.vertex_capacity.max(1);
            while cap < vertices {
                cap *= 2;
            }
            self.vertex_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sonic-wezterm-present-vertices"),
                size: cap * std::mem::size_of::<Vertex>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.vertex_capacity = cap;
            self.record_vertex_request();
        }
        if indices > self.index_capacity {
            let mut cap = self.index_capacity.max(1);
            while cap < indices {
                cap *= 2;
            }
            self.index_buf = create_index_buffer(device, cap, INDEX_BUFFER_USAGE);
            self.index_capacity = cap;
            self.index_pattern_quads = 0;
            self.record_index_request();
        }
    }
}

#[cfg(test)]
#[path = "wezterm_pipeline_buffers_tests.rs"]
mod wezterm_pipeline_buffers_tests;
