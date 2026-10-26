use parking_lot::Mutex;
use sonicterm_types::ResourceOwnerId;
use std::sync::Arc;

/// Process-exit custody for native payloads that cannot safely be destroyed or uncharged.
#[derive(Clone, Default)]
pub struct UnresolvedSink {
    state: Arc<UnresolvedSinkState>,
}

#[derive(Default)]
struct UnresolvedSinkState {
    payloads: Mutex<Vec<UnresolvedEntry>>,
    entries: std::sync::atomic::AtomicUsize,
    duplicates: std::sync::atomic::AtomicUsize,
}

struct UnresolvedEntry {
    owner: ResourceOwnerId,
    _payload: Box<dyn Send>,
}

impl UnresolvedSink {
    /// Retain the whole native payload and its accounting rather than running an incomplete native destructor.
    // Ordering: entries fetch_add Release publishes payload custody; entries snapshots acquire without nesting the payloads lock.
    pub fn retain(&self, owner: ResourceOwnerId, payload: Box<dyn Send>) {
        let mut payloads = self.state.payloads.lock();
        payloads.push(UnresolvedEntry { owner, _payload: payload });
        self.state.entries.fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// Number of unresolved owned payloads still occupying process-exit custody.
    // Ordering: entries Acquire observes completed sink publication without nesting the payloads lock under counters.
    pub fn entries(&self) -> usize {
        self.state.entries.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Count a slotless cancellation duplicate until its wrapper closes the actual native handle.
    pub fn track_fallback_duplicate(&self) -> FallbackDuplicateToken {
        self.state.duplicates.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        FallbackDuplicateToken { state: self.state.clone() }
    }

    /// Open cancellation duplicates owned by slotless cleanup workers, separate from reserved native-handle permits.
    pub fn open_fallback_duplicates(&self) -> usize {
        self.state.duplicates.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(super) fn owners(&self) -> Vec<ResourceOwnerId> {
        self.state.payloads.lock().iter().map(|entry| entry.owner).collect()
    }

    pub(super) fn abandon_payloads(&self) {
        let payloads = std::mem::take(&mut *self.state.payloads.lock());
        // There is no safe native destructor here; preserve payload charges and any self-referential sink clones until OS exit.
        for payload in payloads {
            std::mem::forget(payload);
        }
    }
}

// Lifecycle: UnresolvedSinkState forgets payloads rather than destroying unresolved native resources during terminal drop.
impl Drop for UnresolvedSinkState {
    fn drop(&mut self) {
        for payload in self.payloads.get_mut().drain(..) {
            std::mem::forget(payload);
        }
    }
}

/// Opaque custody token whose lifetime follows one slotless native cancellation duplicate.
pub struct FallbackDuplicateToken {
    state: Arc<UnresolvedSinkState>,
}

// Lifecycle: FallbackDuplicateToken decrements duplicates only after its wrapper releases the native handle.
impl Drop for FallbackDuplicateToken {
    fn drop(&mut self) {
        self.state.duplicates.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "reaper_unresolved_tests.rs"]
mod reaper_unresolved_tests;
