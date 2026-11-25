//! Return a freed pixel buffer's pages to the system.
//!
//! macOS's allocator keeps a freed large allocation's pages dirty until they are marked reusable,
//! so a shrunk glyph atlas's old pixels would still count in the process footprint. Elsewhere a
//! large free already returns its pages, so the buffer is just dropped.

/// Drop `buffer`, first marking every whole page inside it reusable on macOS.
pub(super) fn release_pixel_pages(buffer: Vec<u8>) {
    #[cfg(target_os = "macos")]
    mark_reusable(&buffer);
    drop(buffer);
}

/// The page-aligned range strictly inside `start..start + len` for pages of `page` bytes, or
/// `None` when it holds no whole page.
#[cfg(any(target_os = "macos", test))]
fn whole_pages(start: usize, len: usize, page: usize) -> Option<(usize, usize)> {
    let first = start.checked_next_multiple_of(page)?;
    let end = start.checked_add(len)? / page * page;
    (end > first).then_some((first, end - first))
}

/// Mark the whole pages inside `buffer` reusable, so the system may take them back before the
/// allocator reuses the block. The buffer is dropped right after, so nothing reads them.
#[cfg(target_os = "macos")]
fn mark_reusable(buffer: &[u8]) {
    let page =
        // SAFETY: sysconf takes a constant name and touches no memory.
        unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let Some(page) = usize::try_from(page).ok().filter(|page| page.is_power_of_two()) else {
        // When: sysconf reports no usable page size, the buffer is dropped without the advice.
        return;
    };
    let Some((first, len)) = whole_pages(buffer.as_ptr() as usize, buffer.len(), page) else {
        // When: whole_pages finds no whole page inside the buffer, there is nothing to mark.
        return;
    };
    let result =
        // SAFETY: `first..first + len` is whole pages in `buffer`'s live allocation, dropped next
        // unread; MADV_FREE_REUSABLE only lets the system reclaim them.
        unsafe { libc::madvise(first as *mut libc::c_void, len, libc::MADV_FREE_REUSABLE) };
    // A refused advice only leaves the pages counted until the allocator reuses them.
    if result != 0 {
        tracing::debug!(target: "memory", len, "glyph atlas page release refused");
    }
}

#[cfg(test)]
#[path = "glyph_atlas_page_release_tests.rs"]
mod glyph_atlas_page_release_tests;
