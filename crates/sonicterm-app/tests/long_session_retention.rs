//! Does a long-lived pane return to baseline, or ratchet?
//!
//! #888 recorded its artifact as **"stateful and long-session-only"**: absent
//! from a fresh window, appearing only after extended use, and on a process
//! that reached 51.1 GB before a fatal `wgpu` OOM. The renderer churn baseline
//! covers the other shape — windows opened and closed — and says nothing about
//! one window used for a long time.
//!
//! "Long session" in wall-clock terms is a proxy for **many state
//! transitions**, and those are drivable. This drives them: scrollback
//! churning past its limit, hyperlinks interned and scrolled away, alt-screen
//! entered and left, media captures opened and abandoned, all against one pane
//! that is never recreated.
//!
//! **The assertion is on the shape, not a threshold.** A leak of a few KiB per
//! cycle is invisible against any absolute bound but obvious as a staircase,
//! so retention is sampled every cycle and the second half of the run is
//! compared against the first. A run that ends where its midpoint was is flat;
//! one that ends materially higher is ratcheting, whatever its absolute size.
//!
//! Cross-platform on purpose. The incident was Windows, but nothing here needs
//! a window or a GPU — it drives `App` state — and a leak in pane retention
//! would be as real on macOS. Running it on both is free coverage.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

/// Enough cycles that a per-cycle leak compounds past any single-sample noise,
/// while staying inside a CI test's time budget.
const CYCLES: usize = 900;

/// Bytes retained by the pane after each cycle.
///
/// Generic over the window-id type rather than naming it: `WindowId` is
/// crate-private, and the existing integration tests let it infer for the same
/// reason.
fn drive_session<W: Copy>(
    app: &mut App,
    window: W,
    pane_id: u64,
    advance: impl Fn(&App, W, u64, &[u8]),
    measure: impl Fn(&App, W, u64) -> usize,
) -> Vec<usize> {
    let mut samples = Vec::with_capacity(CYCLES);

    for cycle in 0..CYCLES {
        // Ordinary output, well past the scrollback limit so rows are
        // continuously evicted rather than merely accumulated.
        for line in 0..200 {
            let text = format!("cycle {cycle} line {line} with enough text to occupy cells\r\n");
            advance(app, window, pane_id, text.as_bytes());
        }

        // Hyperlinks interned and then scrolled away. The registry reclaims
        // unreachable entries; one that failed to would show here.
        for index in 0..64 {
            let osc = format!(
                "\x1b]8;;https://example.com/cycle/{cycle}/link/{index}\x07link\x1b]8;;\x07\r\n"
            );
            advance(app, window, pane_id, osc.as_bytes());
        }

        // Alt screen in and out: allocates a saved primary and releases it.
        advance(app, window, pane_id, b"\x1b[?1049h");
        advance(app, window, pane_id, b"alt screen content\r\n");
        advance(app, window, pane_id, b"\x1b[?1049l");

        // A media capture opened and abandoned without its terminator — the
        // shape a killed transfer leaves. Staging must not survive the cycle.
        let mut capture = Vec::with_capacity(64 * 1024 + 3);
        capture.extend_from_slice(b"\x1b_G");
        capture.resize(64 * 1024, b'A');
        advance(app, window, pane_id, &capture);
        // Cancel it the way a stream does, so the next cycle starts clean.
        advance(app, window, pane_id, &[0x18]);

        // A wide grapheme run, which allocates rare-attribute boxes.
        advance(app, window, pane_id, "中文字符测试\r\n".as_bytes());

        samples.push(measure(app, window, pane_id));
    }

    samples
}

/// A long-lived pane's retention is bounded, and its reclamation really runs.
///
/// Named for what it asserts. "Returns to baseline" would be wrong: retention
/// is a sawtooth, because the hyperlink registry reclaims when it fills rather
/// than per link — sweeping the grid on every OSC 8 would be quadratic — so
/// the figure climbs, drops, and climbs again. What is true is that it stays
/// under a ceiling and that the drops are real sweeps.
#[test]
fn a_long_lived_pane_stays_bounded_and_its_reclamation_runs() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["soak"]);
    let pane_id = *app
        .__test_child_pane_ids(window)
        .expect("the seeded window exists")
        .first()
        .expect("the window has a pane");

    let samples = drive_session(
        &mut app,
        window,
        pane_id,
        |app, w, p, bytes| {
            app.__test_advance_child_pane_parser(w, p, bytes);
        },
        |app, w, p| {
            app.__test_pane_retention(w, p).expect("an uncontended pane measures").total().bytes
        },
    );
    assert_eq!(samples.len(), CYCLES, "every cycle must contribute a sample");

    let peak = *samples.iter().max().expect("non-empty");
    // A drop between consecutive samples is reclamation firing. The registry
    // reclaims when it fills rather than per link — sweeping the grid on every
    // OSC 8 would be quadratic — so retention is a sawtooth, and the trough
    // after a sweep is the evidence that the sweep happened.
    let reclamations = samples.windows(2).filter(|w| w[1] < w[0]).count();
    let deepest_drop =
        samples.windows(2).filter(|w| w[1] < w[0]).map(|w| w[0] - w[1]).max().unwrap_or(0);

    println!(
        "cycles={CYCLES} peak={peak} reclamations={reclamations} deepest_drop={deepest_drop} \
         first={} last={}",
        samples[0],
        samples[CYCLES - 1]
    );

    // A *substantial* drop, not merely any drop. Falsification made this
    // necessary: with reclamation disabled the run still showed four
    // "reclamations" of ~10 KiB each, which are allocator slack between
    // samples, not sweeps. Counting those as evidence let the disabled build
    // pass. A real sweep frees the registry — megabytes — so the size of the
    // drop is what distinguishes a sweep from noise, and the count alone
    // cannot.
    const SWEEP_FLOOR: usize = 1024 * 1024;
    assert!(
        deepest_drop >= SWEEP_FLOOR,
        "the deepest drop across the run was {deepest_drop} bytes, below the {SWEEP_FLOOR}-byte \
         floor that distinguishes a reclamation sweep from allocator slack. With reclamation \
         disabled this run still shows small drops, so a test satisfied by any drop proves \
         nothing. Samples: {samples:?}"
    );

    const RETENTION_CEILING: usize = 8 * 1024 * 1024;
    assert!(
        peak <= RETENTION_CEILING,
        "a long-lived pane peaked at {peak} bytes against a {RETENTION_CEILING}-byte ceiling \
         across {CYCLES} cycles. #888's artifact was stateful and long-session-only, so \
         unbounded growth here is the shape that incident had. Samples: {samples:?}"
    );
}
