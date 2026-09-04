//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::color::{chrome_color_to_linear_rgba, ChromeColor};
use sonicterm_engine::FontStack;

pub(crate) static TRACKED_FONT_STACK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn tracked_font_stack(font_size: f64) -> FontStack {
    let assets = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        vec![assets],
        font_size,
        72,
        1.0,
    )
    .expect("bundled test font must load")
}

#[test]
fn exports_color_conversion_helpers() {
    let rgba = chrome_color_to_linear_rgba(ChromeColor::rgb(255, 0, 0));
    assert_eq!(rgba[0], 1.0);
    assert_eq!(rgba[1], 0.0);
    assert_eq!(rgba[2], 0.0);
    assert_eq!(rgba[3], 1.0);
}

/// The legacy alpha-only pipeline retains its source-compatible callable API.
#[test]
fn legacy_text_pipeline_api_remains_callable() {
    use crate::text_pipeline::{GlyphInstance, TextPipeline};
    use wgpu::{BindGroup, Device, Queue, RenderPass, TextureFormat};

    fn draw<'pass>(
        pipeline: &'pass mut TextPipeline,
        device: &Device,
        queue: &Queue,
        pass: &mut RenderPass<'pass>,
        bind_group: &'pass BindGroup,
        instances: &[GlyphInstance],
    ) {
        pipeline.draw(device, queue, pass, bind_group, instances);
    }

    let _: fn(&Device, TextureFormat, u64) -> TextPipeline = TextPipeline::new;
    let _: fn(&TextPipeline) -> u64 = TextPipeline::capacity;
    let _ = draw;
}
