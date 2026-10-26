//! Shared gated native-font fixtures for asynchronous frame-shaping contracts.

use super::*;
use sonicterm_font::locator::{FontDataHandle, FontDataSource, FontLocator, FontOrigin};
use sonicterm_font::parser::ParsedFont;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long the locator waits for its gate, and how long a test waits for the worker.
const WORKER_WAIT: Duration = Duration::from_secs(10);

type Gate = Arc<(Mutex<bool>, Condvar)>;

fn open(gate: &Gate) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

/// Answers every fallback request with Rec Mono once `gate` opens; an unopened gate fails
/// the request instead of hanging the worker.
struct GatedRecMono {
    gate: Gate,
}

impl FontLocator for GatedRecMono {
    fn load_fonts(
        &self,
        _: &[config::FontAttributes],
        _: &mut std::collections::HashSet<config::FontAttributes>,
        _: u16,
    ) -> anyhow::Result<Vec<ParsedFont>> {
        Ok(Vec::new())
    }

    fn locate_fallback_for_codepoints(&self, _: &[char]) -> anyhow::Result<Vec<ParsedFont>> {
        let (opened, changed) = &*self.gate;
        let opened = changed
            .wait_timeout_while(opened.lock().unwrap(), WORKER_WAIT, |opened| !*opened)
            .unwrap()
            .0;
        anyhow::ensure!(*opened, "the test never opened the fallback gate");
        let handle = FontDataHandle {
            source: FontDataSource::OnDisk(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../sonicterm-font/test-fonts/rec-mono/RecMonoSt.Helens-Regular.ttf"),
            ),
            index: 0,
            variation: 0,
            origin: FontOrigin::BuiltIn,
            coverage: None,
        };
        Ok(vec![ParsedFont::from_locator(&handle)?])
    }
}

/// A stack whose only primary face is the ASCII-only sample font (family Roboto), so é reaches
/// the gated locator; the temporary font directory is removed on drop.
pub(super) struct GatedStack {
    pub(super) stack: FontStack,
    pub(super) gate: Gate,
    directory: PathBuf,
}

impl Drop for GatedStack {
    // Lifecycle: dropping `GatedStack` removes its temporary font `directory`.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

pub(super) fn gated_stack(name: &str) -> GatedStack {
    let directory =
        std::env::temp_dir().join(format!("sonicterm-engine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../sonicterm-harfbuzz/harfbuzz/src/wasm/sample/c/test.ttf"),
        directory.join("primary.ttf"),
    )
    .unwrap();
    let gate: Gate = Arc::default();
    let stack = FontStack::try_new_with_locator_for_test(
        "Roboto",
        vec![directory.clone()],
        Arc::new(GatedRecMono { gate: Arc::clone(&gate) }),
        14.0,
        96,
    )
    .unwrap();
    GatedStack { stack, gate, directory }
}

pub(super) fn wait_for_generation(stack: &FontStack, generation: u64) {
    let started = Instant::now();
    while stack.fallback_notice().generation() < generation {
        assert!(started.elapsed() < WORKER_WAIT, "generation {generation} was never published");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait, bounded, until `frames` has received `count` wakes. The notice bumps its generation
/// before it posts the wake, so a test that saw the generation must still wait for delivery.
fn wait_for_wakes(frames: &Frames<'_>, count: usize) {
    let started = Instant::now();
    while frames.wakes() < count {
        assert!(started.elapsed() < WORKER_WAIT, "{count} wake(s) were never delivered");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait(gate: &Gate) {
    let (opened, changed) = &**gate;
    let opened = changed
        .wait_timeout_while(opened.lock().unwrap(), WORKER_WAIT, |opened| !*opened)
        .unwrap()
        .0;
    assert!(*opened, "a test gate was never opened");
}

/// A pause point for the worker: it opens `entered`, then waits until `release` opens.
fn pause_hook(entered: &Gate, release: &Gate) -> Arc<dyn Fn() + Send + Sync> {
    let (entered, release) = (Arc::clone(entered), Arc::clone(release));
    Arc::new(move || {
        open(&entered);
        wait(&release);
    })
}

/// The renderer's frame-font contract in miniature: `begin` applies a newer notice generation
/// once per frame and drops the stored title width; `title_width` measures only when nothing
/// is stored; `wake_due` is the handler acknowledging the notice. The renderer's own seam
/// (`prepare_frame_fonts`, `fallback_frame_due`) is tested in the GPU crate; this drives the
/// same rules against the production frame entry points and a real worker.
struct Frames<'stack> {
    stack: &'stack FontStack,
    applied: Option<(u64, u64)>,
    applies: usize,
    title_width: Option<f32>,
    wakes: Arc<std::sync::atomic::AtomicUsize>,
}

impl<'stack> Frames<'stack> {
    fn new(stack: &'stack FontStack) -> Self {
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        stack.fallback_notice().attach_waker(Arc::new(move |_notice| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        Self { stack, applied: None, applies: 0, title_width: None, wakes }
    }

    fn begin(&mut self) -> bool {
        let notice = self.stack.fallback_notice();
        let current = (notice.id(), notice.generation());
        if self.applied == Some(current) {
            // When: `applied` already holds this generation, nothing can be stale.
            return false;
        }
        self.applied = Some(current);
        self.applies += 1;
        self.title_width = None;
        true
    }

    fn title_width(&mut self) -> f32 {
        let stack = self.stack;
        *self.title_width.get_or_insert_with(|| stack.measure_text_width_for_frame("é").unwrap())
    }

    fn wake_due(&self) -> bool {
        let notice = self.stack.fallback_notice();
        let generation = notice.acknowledge();
        self.applied != Some((notice.id(), generation))
    }

    fn wakes(&self) -> usize {
        self.wakes.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The glyph id and advance a frame shapes for é.
fn shaped_e(stack: &FontStack) -> (u32, f64) {
    let shaped = stack.shape_text_for_frame("é", false, false).unwrap();
    (shaped[0].glyph_pos, shaped[0].x_advance.get())
}

#[cfg(test)]
#[path = "fontstack_frame_fixture_tests.rs"]
mod fontstack_frame_fixture_tests;
