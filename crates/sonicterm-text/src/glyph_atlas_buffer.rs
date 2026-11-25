//! The glyph atlas's pixel storage.
//!
//! A large buffer is an anonymous memory mapping on macOS, so dropping it (when the atlas grows or
//! shrinks) unmaps its pages at once. The system allocator keeps a freed large block's pages dirty
//! in its cache, where they stay in the process footprint. Small buffers, and every buffer on
//! other platforms, whose allocators already return large frees, are ordinary vectors.

use std::ops::{Deref, DerefMut};

/// Buffers at least this large are mapped on macOS; below it the allocator's small zones hold them.
#[cfg(target_os = "macos")]
const MAPPED_FROM_BYTES: usize = 1024 * 1024;

/// Zero-initialized atlas pixels that read and write as one byte slice.
pub(super) enum AtlasPixels {
    /// Heap storage.
    Heap(Vec<u8>),
    /// An anonymous mapping, unmapped when dropped.
    #[cfg(target_os = "macos")]
    Mapped(memmap2::MmapMut),
}

impl AtlasPixels {
    /// `len` zero bytes: mapped on macOS from [`MAPPED_FROM_BYTES`], else on the heap. A mapping
    /// the system refuses falls back to the heap.
    pub(super) fn zeroed(len: usize) -> Self {
        #[cfg(target_os = "macos")]
        if len >= MAPPED_FROM_BYTES {
            // When: len reaches MAPPED_FROM_BYTES, a mapping (zero-filled by the system) is tried first.
            if let Ok(mapping) = memmap2::MmapMut::map_anon(len) {
                // When: map_anon succeeds, the buffer is the mapping; a refusal falls through to the heap.
                return Self::Mapped(mapping);
            }
        }
        Self::Heap(vec![0; len])
    }

    /// Bytes this buffer holds in reserve: a vector's capacity, or a mapping's length.
    pub(super) fn capacity(&self) -> usize {
        match self {
            Self::Heap(bytes) => bytes.capacity(),
            #[cfg(target_os = "macos")]
            Self::Mapped(mapping) => {
                // When: the buffer is Mapped, its whole length is reserved, so that is its capacity.
                mapping.len()
            }
        }
    }

    /// Whether this buffer is a mapping rather than heap storage.
    #[cfg(test)]
    pub(super) fn is_mapped(&self) -> bool {
        !matches!(self, Self::Heap(_))
    }
}

impl Deref for AtlasPixels {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Heap(bytes) => bytes,
            #[cfg(target_os = "macos")]
            Self::Mapped(mapping) => {
                // When: the buffer is Mapped, the mapping itself is the byte slice.
                mapping
            }
        }
    }
}

impl DerefMut for AtlasPixels {
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            Self::Heap(bytes) => bytes,
            #[cfg(target_os = "macos")]
            Self::Mapped(mapping) => {
                // When: the buffer is Mapped, the mapping itself is the writable byte slice.
                mapping
            }
        }
    }
}

#[cfg(test)]
#[path = "glyph_atlas_buffer_tests.rs"]
mod glyph_atlas_buffer_tests;
