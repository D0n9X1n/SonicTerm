//! Native renderer coverage stays in the harness test process and its one event loop.

use super::*;

/// Once fallback completes on a presented frame, the helper's identity set covers every tile
/// the real renderer holds for S9 and S12 with the tab bar, titles, cursor and an open
/// palette, at scale 1 and 2, with the same raster sizes, and S9's representative emoji and CJK
/// are resident real tiles; the module comment states what this proves and what it does not.
#[test]
fn the_helper_covers_the_real_renderer() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("working-set coverage event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
