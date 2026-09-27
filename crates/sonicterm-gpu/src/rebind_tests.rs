use super::*;

/// The text of `rebind.rs`'s `fn name`, from its signature to its closing brace.
fn method(name: &str) -> String {
    let source = include_str!("rebind.rs").replace("\r\n", "\n");
    let header = format!("fn {name}(");
    let start = source.find(&header).unwrap_or_else(|| panic!("missing `{name}`"));
    let end = source[start..].find("\n    }\n").unwrap_or_else(|| panic!("unterminated `{name}`"));
    source[start..start + end].to_owned()
}

/// A candidate needing the window's sRGB format and premultiplied native alpha.
fn needs(degrade: bool) -> SurfaceNeeds {
    SurfaceNeeds {
        format: TextureFormat::Bgra8UnormSrgb,
        alpha_mode: CompositeAlphaMode::PreMultiplied,
        degrade,
    }
}

const FORMATS: &[TextureFormat] = &[TextureFormat::Bgra8UnormSrgb];
const ALPHAS: &[CompositeAlphaMode] =
    &[CompositeAlphaMode::PreMultiplied, CompositeAlphaMode::Opaque];
const BOTH: &[PresentMode] = &[PresentMode::Mailbox, PresentMode::Fifo];
const FIFO: &[PresentMode] = &[PresentMode::Fifo];
const DRAW: TextureUsages = TextureUsages::RENDER_ATTACHMENT;

/// A candidate that keeps the format and effective alpha gets Mailbox when offered
/// and Fifo otherwise; degrade keeps Mailbox as the hardware mode but requires the
/// Fifo it presents with, and a surface offering no usable mode is refused.
#[test]
fn present_mode_prefers_mailbox_and_requires_the_mode_in_effect() {
    let mode = |needs, modes| supported_present_mode(needs, FORMATS, modes, ALPHAS, DRAW);
    assert_eq!(mode(needs(false), BOTH).unwrap(), PresentMode::Mailbox);
    assert_eq!(mode(needs(false), FIFO).unwrap(), PresentMode::Fifo);
    assert_eq!(mode(needs(true), BOTH).unwrap(), PresentMode::Mailbox);
    assert!(mode(needs(true), &[PresentMode::Mailbox]).is_err());
    assert!(mode(needs(false), &[]).is_err());
}

/// A candidate that would change the window's colors or backdrop, or cannot be drawn
/// into, is refused rather than adapted: a missing format, a missing effective alpha
/// (the native alpha, or opaque while degraded), or no render-attachment use.
#[test]
fn candidate_that_cannot_keep_the_window_look_is_refused() {
    let linear = [TextureFormat::Bgra8Unorm];
    assert!(supported_present_mode(needs(false), &linear, BOTH, ALPHAS, DRAW).is_err());
    let opaque = [CompositeAlphaMode::Opaque];
    assert!(supported_present_mode(needs(false), FORMATS, BOTH, &opaque, DRAW).is_err());
    assert!(supported_present_mode(needs(true), FORMATS, BOTH, &opaque, DRAW).is_ok());
    let native = [CompositeAlphaMode::PreMultiplied];
    assert!(supported_present_mode(needs(true), FORMATS, BOTH, &native, DRAW).is_err());
    let copy = TextureUsages::COPY_SRC;
    assert!(supported_present_mode(needs(false), FORMATS, BOTH, ALPHAS, copy).is_err());
}

/// A surface made on another instance or for another window is refused on identity
/// alone, before any capability query.
#[test]
fn candidate_from_another_instance_or_window_is_refused() {
    assert!(candidate_identity(&1_u8, &1, 7_u32, 7).is_ok());
    let instance = candidate_identity(&2_u8, &1, 7_u32, 7).unwrap_err().to_string();
    assert!(instance.contains("another instance"), "{instance}");
    let window = candidate_identity(&1_u8, &1, 8_u32, 7).unwrap_err().to_string();
    assert!(window.contains("another window"), "{window}");
}

/// Preparation checks the candidate's instance and window before it asks the adapter
/// or the surface anything, then checks support and fresh capabilities before it
/// builds a resource; commit repeats the identity check under its gate.
#[test]
fn identity_and_support_come_before_capabilities_and_resources() {
    let prepare = method("prepare_rebind");
    let at = |needle: &str| prepare.find(needle).unwrap_or_else(|| panic!("missing `{needle}`"));
    let gate = at(".enter_gpu_work(\"recovery.prepare\")");
    let identity = at("candidate_identity(");
    let supported = at(".is_surface_supported(&surface.surface)");
    let capabilities = at(".get_capabilities(&context.adapter)");
    let modes = at("supported_present_mode(");
    let build = at("WeztermPipeline::new(&context.device");
    assert!(gate < identity && identity < supported && supported < capabilities);
    assert!(capabilities < modes && modes < build);
    assert_eq!(prepare[..identity].matches("surface.surface").count(), 0);
    let commit = method("commit_rebind");
    let at = |needle: &str| commit.find(needle).unwrap_or_else(|| panic!("missing `{needle}`"));
    assert!(at(".enter_gpu_work(\"recovery.commit\")") < at("candidate_identity("));
    assert!(at("candidate_identity(") < at("self.surface = surface.surface;"));
}

/// A failed preparation cannot change geometry or renderer state: it borrows the
/// renderer immutably and assigns nothing, and only commit writes the geometry, after
/// its gate.
#[test]
fn failed_preparation_mutates_no_renderer_state() {
    let prepare = method("prepare_rebind");
    assert!(prepare.contains("&self,") && !prepare.contains("&mut self"));
    let writes = prepare
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("self.") && line.contains(" = "))
        .count();
    assert_eq!(writes, 0, "preparation must assign no renderer field");
    let commit = method("commit_rebind");
    let gate = commit.find(".enter_gpu_work(\"recovery.commit\")").expect("commit gate");
    let geometry = commit.find("self.config.width = width;").expect("geometry write");
    assert!(gate < geometry, "geometry changes only after the commit gate");
}

/// Commit resets every cache and flag tied to the old device's objects, and keeps the
/// renderer's counters, fonts, and settings; it neither redraws nor destroys.
#[test]
fn commit_resets_the_device_inventory_and_keeps_renderer_state() {
    let reset = method("reset_after_rebind");
    for step in [
        "self.reset_glyph_atlas_in_place(\"device_recovery\")",
        "self.reset_image_atlas()",
        "self.row_glyph_cache.invalidate_all()",
        "self.line_quad_cache.invalidate_all()",
        "self.glyph_atlas_retry_without_eviction = false",
        "self.fault_invalid_glyph_upload = false",
        "self.fault_frame_probe = None",
        "self.fault_stop_before_cached_present = false",
        "self.software_frame = None",
        "self.device_stop_reported = false",
        "self.last_frame_key = None",
        "self.last_pane_layout.clear()",
    ] {
        assert!(reset.contains(step), "reset misses `{step}`");
    }
    let commit = method("commit_rebind");
    assert!(commit.contains("self.reset_after_rebind()"));
    for kept in [
        "present_calls",
        "successful_frame_count",
        "LIVE_RENDERERS",
        "font_",
        "request_redraw",
        "destroy(",
    ] {
        assert!(
            !commit.contains(kept) && !reset.contains(kept),
            "commit must leave `{kept}` alone"
        );
    }
}

/// With the retained key cleared, the next plan has no previous key, and the frame
/// plan derives a full first frame from exactly that absence.
#[test]
fn cleared_frame_key_forces_a_full_first_frame() {
    assert!(method("reset_after_rebind").contains("self.last_frame_key = None"));
    let plan = include_str!("frame_plan.rs").replace("\r\n", "\n");
    assert!(plan.contains("let first_frame = previous.is_none();"));
}

/// Commit opens the candidate's gate and checks identity before it changes anything,
/// replaces the old surface (whose drop unconfigures it) before it configures the new
/// one, keeps the native alpha, reads the gate after that configure, and never polls.
#[test]
fn commit_drops_the_old_surface_before_configuring_the_new_one() {
    let commit = method("commit_rebind");
    let at = |needle: &str| commit.find(needle).unwrap_or_else(|| panic!("missing `{needle}`"));
    let gate = at(".enter_gpu_work(\"recovery.commit\")");
    let replace = at("self.surface = surface.surface;");
    let device = at("self.device = context.device;");
    let geometry = at("self.config.width = width;");
    let configure = at("self.surface.configure(&self.device, &self.config);");
    let reading = at("Ok(self.device_errors.gate())");
    assert!(gate < replace && replace < device && device < geometry);
    assert!(geometry < configure && configure < reading);
    assert!(commit.contains("self.hardware_alpha_mode"));
    assert!(!commit.contains("self.hardware_alpha_mode =") && !commit.contains(".poll("));
}

/// The candidate names that `body` uses before its prepare gate.
fn candidate_work_before_gate(body: &str) -> Vec<&'static str> {
    let gate = body.find(".enter_gpu_work(\"recovery.prepare\")").expect("prepare gate");
    ["context.device.", "context.device,", "context.device)", "context.queue", "context.adapter"]
        .into_iter()
        .filter(|needle| body[..gate].contains(needle))
        .collect()
}

/// Preparation builds every device-bound object on the candidate only after its gate,
/// from the window's current size, the caller's current mode, and the renderer's own
/// format and native alpha, and leaves the renderer untouched: it resets and
/// invalidates nothing, uses no size-skipping rebuild helper, and requests no redraw.
/// A build seeded before the gate fails the check.
#[test]
fn prepare_builds_on_the_candidate_after_its_gate_without_mutation() {
    let prepare = method("prepare_rebind");
    assert_eq!(candidate_work_before_gate(&prepare), Vec::<&str>::new());
    for build in [
        "WeztermPipeline::new(&context.device",
        "create_frame_texture(&context.device",
        "TextureBlitter::new(&context.device",
        "AtlasBindingKind::Glyph",
        "AtlasBindingKind::Image",
        "self.window.inner_size()",
        "software_render_degrade_from(",
        "alpha_mode: self.hardware_alpha_mode",
        "format: self.config.format",
    ] {
        assert!(prepare.contains(build), "prepare misses `{build}`");
    }
    for forbidden in
        ["reset_", "_upload_if_needed", "invalidate_all", "request_redraw", "self.config.width ="]
    {
        assert!(!prepare.contains(forbidden), "prepare must not use `{forbidden}`");
    }
    let seeded = prepare.replacen(
        "let errors = &context.device_errors;",
        "let early = WeztermPipeline::new(&context.device, self.config.format, 1);\n        let errors = &context.device_errors;",
        1,
    );
    assert!(!candidate_work_before_gate(&seeded).is_empty());
}
