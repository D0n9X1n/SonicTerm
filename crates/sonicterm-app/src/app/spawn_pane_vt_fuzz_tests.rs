//! Seeded image-protocol fragments through the real VT batch publisher and the real decoder:
//! whatever Kitty, iTerm2 and Sixel bytes arrive, whole or cut short, the pane's media stays within
//! its private pool's budget and every reply is delivered rather than dropped.

use super::super::spawn_pane_tests::*;
use super::*;
use base64::Engine as _;

/// A one-pixel PNG, so some fragments decode into a real image.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52, 0, 0, 0,
    1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1F, 0x15, 0xC4, 0x89, 0, 0, 0, 13, 0x49, 0x44, 0x41, 0x54,
    0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0, 0x1F, 0, 0x05, 0, 0x01, 0xFF, 0x5B, 0x6E, 0x3E, 0x41,
    0, 0, 0, 0, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// One seeded fragment: a whole or truncated Kitty, iTerm2 or Sixel sequence, or noise.
fn fragment(seed: &mut u64) -> Vec<u8> {
    let mut next = |bound: u64| {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed % bound
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(PNG);
    let mut bytes = match next(5) {
        0 => format!("\x1b_Gf=100,a=T,i={};{encoded}\x1b\\", next(4) + 1).into_bytes(),
        1 => format!("\x1b_Ga=T,f=32,s={},v={};AAAA\x1b\\", next(5000), next(5000)).into_bytes(),
        2 => format!("\x1b]1337;File=inline=1;size={}:{encoded}\x07", PNG.len()).into_bytes(),
        3 => format!("\x1bPq\"1;1;{};{}#0~-~\x1b\\", next(3000), next(3000)).into_bytes(),
        _ => (0..next(200)).map(|_| next(256) as u8).collect(),
    };
    // Cut a third of the fragments short, as a reader would deliver a partial frame.
    if next(3) == 0 {
        bytes.truncate(next(bytes.len() as u64 + 1) as usize);
    }
    bytes.extend_from_slice(b"text\r\n");
    bytes
}

/// Seeded media fragments through `publish_pane_vt_batch_with` and `decode_inline_image` keep
/// the pane's retained media within its pool budget and never drop a reply.
#[test]
fn media_fragments_stay_within_the_pool_budget() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    )
    .with_capture_staging_pool(sonicterm_vt::vt::CaptureStagingPool::new())
    .with_inline_media_pool(crate::app::media::InlineMediaPool::new());
    app.__test_seed_tab("main");
    let main = app.registry.main_window_id.unwrap();
    let pane_id = app.registry.windows[&main].tab_states[0].active_pane;
    let pane = &app.registry.windows[&main].panes[&pane_id];
    let handles = PaneVtHandles::from_pane_state(pane);
    let pool = Arc::clone(&app.inline_media_pool);
    let mut sync_latch = SyncLatch::default();
    let mut seed = 0xFEED_FACE_u64;
    let mut replies = 0;
    let mut most_images = 0;
    for round in 0..400 {
        let batch: Vec<u8> = (0..1 + round % 4).flat_map(|_| fragment(&mut seed)).collect();
        publish_pane_vt_batch_with(
            &handles,
            &batch,
            None,
            &mut None,
            &mut sync_latch,
            crate::app::media::decode_inline_image,
            |_| {},
            test_base,
            |reply: Vec<u8>| replies += reply.len(),
        );
        assert!(
            pool.bytes() <= pool.pane_budget() + crate::app::media::MAX_SINGLE_INLINE_IMAGE_BYTES,
            "round {round}: {} retained against a budget of {}",
            pool.bytes(),
            pool.pane_budget()
        );
        most_images = most_images.max(handles.inline_images.lock().len());
    }
    assert!(most_images > 0, "some fragments decoded into retained images");
    assert!(replies > 0, "Kitty acknowledgements reached the reply path");
}
