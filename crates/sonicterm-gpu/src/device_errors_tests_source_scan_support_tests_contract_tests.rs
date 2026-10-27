//! Renderer device-gate source contracts and seeded violation controls.

use super::*;

/// Every renderer method that reaches the device runs that work where the
/// gate's result controls it, or only reads the device; Drop issues no GPU
/// work.
///
/// The check is textual. It proves where handle uses and method calls sit
/// relative to a `gpu_work` closure, an `if let` gate body, or the code after a
/// `let`-`else` or propagated gate. It cannot see what a helper outside
/// `impl GpuRenderer` does with an object the renderer already holds, such as a
/// pipeline or an upload, so such a call is checked only when it passes a handle.
#[test]
fn renderer_gpu_work_goes_through_the_gate() {
    let files = production_sources();
    let sources: Vec<&str> = files.iter().map(|(_, text)| text.as_str()).collect();
    let violations = gate_violations(&sources);
    assert!(violations.is_empty(), "device-gate violations: {violations:?}");
    let core = code_only(crate::core::CORE_SOURCE);
    let drop_block = block_after(&core, "impl Drop for GpuRenderer {");
    assert!(
        handle_uses(&drop_block).is_empty() && !drop_block.contains("wgpu::"),
        "renderer Drop must issue no GPU work"
    );
}

/// Outside tests and the fault hook's two helpers, every production source
/// under `src/` never polls, never uses render bundles or buffer-init helpers,
/// and pushes no error scope.
#[test]
fn production_code_avoids_fatal_wgpu_paths() {
    let mut offenders = Vec::new();
    for (name, mut text) in production_sources() {
        if name == "device_errors.rs" {
            text = without_function(&text, "pub fn destroy_and_await_loss(");
            text = without_function(&text, "pub fn run_isolated_validation(");
        }
        offenders.extend(banned_calls(&name, &text));
    }
    assert!(offenders.is_empty(), "fatal-path wgpu calls in production code: {offenders:?}");
}

/// Mutation controls: the structural checks pass the renderer as written, and
/// fail on an ungated method in a second `impl GpuRenderer` block, a discarded
/// `accepts_gpu_work()` or gate scope, work outside or before the gate, a
/// `poll_all` call, and a source with no renderer block.
#[test]
fn structural_checks_reject_seeded_defects() {
    let renderer = [
        crate::core::CORE_SOURCE,
        include_str!("present.rs"),
        include_str!("rebind.rs"),
        include_str!("recovery_context.rs"),
        include_str!("atlas_lifecycle.rs"),
    ]
    .join("\n");
    assert_eq!(gate_violations(&[renderer.as_str()]), Vec::<String>::new());
    let seeded = |name: &str, body: &str| {
        let method = format!("    fn {name}(&self) {{\n        {body}\n    }}\n");
        format!("{renderer}\nimpl GpuRenderer {{\n{method}}}\n")
    };
    let submit = "self.queue.submit(None);";
    let gate = "self.device_errors.enter_gpu_work(\"seed\")";
    let defects = [
        ("second_block_work", submit.to_owned()),
        ("discarded_check", format!("let _ = self.device_errors.accepts_gpu_work(); {submit}")),
        ("discarded_scope", format!("let _scope = {gate}; {submit}")),
        ("work_outside_closure", format!("self.device_errors.gpu_work(\"x\", || ()); {submit}")),
        ("work_before_gate", format!("{submit} let Some(_s) = {gate} else {{ return; }};")),
        ("seeded_poll", "wgpu::Instance::poll_all(&self.instance, true);".to_owned()),
        ("seeded_poll_method", "self.instance.poll_all(true);".to_owned()),
    ];
    for (name, body) in &defects {
        let source = seeded(name, body.as_str());
        let violations = gate_violations(&[source.as_str()]);
        assert!(
            violations.iter().any(|violation| violation.contains(*name)),
            "{name}: {violations:?}"
        );
        if name.starts_with("seeded_poll") {
            let offenders = banned_calls("seeded.rs", &source);
            assert!(offenders.iter().any(|offender| offender.ends_with("poll_all(")), "{name}");
        }
    }
    let gated_body = format!("let Some(_s) = {gate} else {{ return; }}; {submit}");
    let gated = seeded("seeded_gated", gated_body.as_str());
    assert_eq!(gate_violations(&[gated.as_str()]), Vec::<String>::new());
    // An extracted phase is still in the transitive assembly graph, not a source-scan blind spot.
    let phase = include_str!("core_assemble_glyph_rows.rs");
    let anchor = phase.find("pub(super) fn assemble_glyph_rows(").expect("real phase");
    let body = anchor + phase[anchor..].find('{').expect("phase body") + 1;
    let mut seeded_phase = phase.to_owned();
    seeded_phase.insert_str(body, " self.queue.submit(None); ");
    let seeded_renderer = renderer.replace(phase, &seeded_phase);
    assert_ne!(seeded_renderer, renderer, "the complete corpus contains the real phase");
    let violations = gate_violations(&[&seeded_renderer]);
    for method in ["assemble_glyph_rows", "assemble_frame"] {
        assert!(
            violations.iter().any(|violation| violation.contains(method)),
            "{method}: {violations:?}"
        );
    }
    let missing = gate_violations(&["fn unrelated() {}"]);
    assert_eq!(missing, vec!["no `impl GpuRenderer` block".to_owned()]);
}

/// CRLF checkout text must have exactly the same extracted blocks, retained
/// suffix and violations as LF, including banned work after an exempt hook.
#[test]
fn structural_checks_are_identical_for_lf_and_crlf() {
    let core = [
        crate::core::CORE_SOURCE,
        include_str!("present.rs"),
        include_str!("rebind.rs"),
        include_str!("recovery_context.rs"),
        include_str!("atlas_lifecycle.rs"),
    ]
    .join("\n")
    .replace("\r\n", "\n");
    let windows = core.replace('\n', "\r\n");
    assert_eq!(code_only(&core), code_only(&windows));
    assert_eq!(gate_violations(&[&core]), gate_violations(&[&windows]));
    let header = "impl Drop for GpuRenderer {";
    assert_eq!(block_after(&code_only(&core), header), block_after(&code_only(&windows), header));
    assert_eq!(block_after(&core, header), block_after(&windows, header));
    let source =
        "pub fn exempt() {\n    device.poll();\n}\nfn later() {\n    instance.poll_all(true);\n}\n";
    let lf = without_function(source, "pub fn exempt(");
    let crlf = without_function(&source.replace('\n', "\r\n"), "pub fn exempt(");
    assert_eq!(lf, crlf);
    assert!(lf.contains("fn later()"), "exempting one hook cannot erase later production code");
    assert_eq!(banned_calls("fixture.rs", &lf), banned_calls("fixture.rs", &crlf));
    assert!(banned_calls("fixture.rs", &crlf).iter().any(|error| error.ends_with("poll_all(")));
}

/// A gate in the constructor does not license earlier configure work; a label
/// expression runs before gpu_work can decide whether to invoke its closure.
#[test]
fn constructor_and_gate_arguments_cannot_bypass_containment() {
    let core = [
        crate::core::CORE_SOURCE,
        include_str!("present.rs"),
        include_str!("rebind.rs"),
        include_str!("recovery_context.rs"),
        include_str!("atlas_lifecycle.rs"),
    ]
    .join("\n");
    assert!(gate_violations(&[core.as_str()]).is_empty());
    let anchor = "        let init_scope = errors";
    assert_eq!(core.matches(anchor).count(), 1);
    let ungated = core.replacen(
        anchor,
        "        surface.configure(&device, &config);\n        let init_scope = errors",
        1,
    );
    assert!(gate_violations(&[&ungated]).iter().any(|error| error.contains("new_async")));
    let bootstrap_anchor = "        let format = TextureFormat::Bgra8UnormSrgb;";
    let before_bootstrap = core.replacen(
        bootstrap_anchor,
        &format!("        surface.configure(&device, &config);\n{bootstrap_anchor}"),
        1,
    );
    assert!(gate_violations(&[&before_bootstrap]).iter().any(|error| error.contains("new_async")));
    let argument = concat!(
        "\nimpl GpuRenderer {\n    fn argument_work(&self) {\n",
        "        self.device_errors.gpu_work({self.queue.submit(None); \"probe\"},||());\n",
        "    }\n}\n"
    );
    let seeded = format!("{core}{argument}");
    assert!(gate_violations(&[&seeded]).iter().any(|error| error.contains("argument_work")));
    let body = code_only("self.device_errors.gpu_work(\"probe\", || { self.queue.submit(None); })");
    let ranges = gate_ranges(&body);
    assert!(ranges.iter().any(|range| range.contains(&body.find("self.queue").unwrap())));
}

/// Moving native work out of `render_frame` does not exempt any new entry point:
/// ungated work in a delegate or a presenter must still fail structural validation.
#[test]
fn presentation_delegates_and_presenters_remain_in_the_gate_graph() {
    let renderer = [
        crate::core::CORE_SOURCE,
        include_str!("present.rs"),
        include_str!("rebind.rs"),
        include_str!("recovery_context.rs"),
        include_str!("atlas_lifecycle.rs"),
    ]
    .join("\n");
    assert!(gate_violations(&[renderer.as_str()]).is_empty());
    for name in [
        "render",
        "render_with_outcome",
        "render_releasing",
        "assemble_frame",
        "present_layers",
        "present_frame",
        "prepare_cached_present",
        "present_unchanged_frame",
        "reblit_software_frame",
        "present_software_frame",
        "present_wgpu_frame",
        "finish_surface_retry",
        "probe_surface_availability",
        "invalidate_retained_frame",
        "__occlude_next_surface_acquire",
    ] {
        let (_, methods) = renderer_methods(&[code_only(&renderer)]);
        assert!(methods.iter().any(|method| method.name == name), "missing {name}");
        // A second definition is merged by the scanner, exposing work outside the real gate.
        let seeded = format!(
            "{renderer}\nimpl GpuRenderer {{\n    fn {name}(&self) {{ self.queue.submit(None); }}\n}}\n"
        );
        let violations = gate_violations(&[seeded.as_str()]);
        assert!(
            violations.iter().any(|violation| violation.contains(name)),
            "{name} escaped the gate graph: {violations:?}"
        );
    }
}

/// Startup and recovery request adapters and devices through one function: production
/// code calls `request_adapter` and `request_device` only in `negotiate_device`,
/// which installs the error handlers after the device request and issues no other
/// device work. Comments and split chains are normalized first, so a call on or a borrow
/// of a handle seeded before the handlers fails on one line, across lines, or split by
/// a comment; only the exact install call is exempt, and a name containing a handle
/// passes.
#[test]
fn device_negotiation_has_one_bootstrap_path() {
    let squeeze = |text: &str| text.split_whitespace().collect::<String>();
    let mut requests = Vec::new();
    for (name, text) in production_sources() {
        let code = join_chains(&code_only(&text));
        for call in [".request_adapter(", ".request_device("] {
            requests.extend(code.matches(call).map(|_| format!("{name}: {call}")));
        }
    }
    assert_eq!(
        requests,
        ["recovery_context.rs: .request_adapter(", "recovery_context.rs: .request_device("]
    );
    let core = squeeze(&code_only(crate::core::CORE_SOURCE));
    assert_eq!(core.matches("recovery_context::negotiate_device(&instance,&surface)").count(), 1);
    let context = include_str!("recovery_context.rs");
    let run = "negotiate_device(&surface.instance,&surface.surface)";
    assert_eq!(squeeze(&code_only(context)).matches(run).count(), 1);
    let body = negotiation_body(context);
    assert_eq!(negotiation_work(&body), Vec::<usize>::new());
    let request = body.find(".request_device(").expect("device request");
    let install = body.find("install_device_error_handlers(&device)").expect("handler install");
    assert!(request < install, "the handlers must follow the device request");
    let anchor = "    let device_errors = install_device_error_handlers(&device);";
    assert_eq!(context.matches(anchor).count(), 1);
    for seed in [
        "device.create_buffer(&probe);",
        "device\n        .create_buffer(&probe);",
        "device /* seeded */ .create_buffer(&probe);",
        "queue\n        // seeded\n        .submit(None);",
        "build_frame_texture(&device, false, 1, 1, TextureFormat::Bgra8UnormSrgb);",
        "build_frame_texture(&\n        device, false, 1, 1, TextureFormat::Bgra8UnormSrgb);",
        "build_frame_texture(& /* gap */ device, false, 1, 1, TextureFormat::Bgra8UnormSrgb);",
        "probe(&mut /* gap */\n        queue);",
        "let early = install_device_error_handlers(& /* gap */ device);",
    ] {
        let seeded = context.replacen(anchor, &format!("    {seed}\n{anchor}"), 1);
        assert!(!negotiation_work(&negotiation_body(&seeded)).is_empty(), "{seed:?} must fail");
    }
    let partial = context.replacen(
        anchor,
        &format!("    probe(& /* gap */ device_count, &mut surfaces);\n{anchor}"),
        1,
    );
    assert_eq!(negotiation_work(&negotiation_body(&partial)), Vec::<usize>::new());
}

/// Both checkout newline forms retain exact mutation anchors and still reject ungated candidate work.
#[test]
fn recovery_fixture_normalizes_anchors_before_seeding_lf_and_crlf() {
    let lf = RECOVERY_RENDERER_SOURCES.map(|source| source.replace("\r\n", "\n"));
    let crlf = lf.each_ref().map(|source| source.replace('\n', "\r\n"));
    for sources in [&lf, &crlf] {
        let renderer = recovery_renderer(sources.each_ref().map(String::as_str));
        assert!(gate_violations(&[&renderer]).is_empty());
        for (method, anchor, seed) in [
            (
                "prepare_rebind",
                "        let errors = &context.device_errors;\n",
                "context.device.create_buffer(&probe);",
            ),
            (
                "commit_rebind",
                "        let errors = Arc::clone(&context.device_errors);\n",
                "context.queue.submit(None);",
            ),
            (
                "candidate_surface",
                "        let window = Arc::clone(&self.window);\n",
                "self.queue.submit(None);",
            ),
        ] {
            assert_eq!(renderer.matches(anchor).count(), 1, "{method} checkout anchor");
            let seeded = renderer.replacen(anchor, &format!("        {seed}\n{anchor}"), 1);
            let violations = gate_violations(&[&seeded]);
            assert!(
                violations.iter().any(|violation| violation.contains(method)),
                "{method}: {violations:?}"
            );
        }
    }
}

/// The device-gate graph covers a candidate context's device, queue, and instance and
/// a candidate surface, not only the renderer's own handles: device work seeded
/// before the prepare or commit gate fails, also when the receiver and field are split
/// by a line break, spaces, or a comment.
#[test]
fn candidate_handles_before_a_rebind_gate_fail_the_graph() {
    let renderer = recovery_renderer(RECOVERY_RENDERER_SOURCES);
    assert_eq!(gate_violations(&[renderer.as_str()]), Vec::<String>::new());
    let anchors = [
        ("prepare_rebind", "        let errors = &context.device_errors;\n"),
        ("commit_rebind", "        let errors = Arc::clone(&context.device_errors);\n"),
    ];
    let seeds = [
        "let early = build_frame_texture(&context.device /* seeded */, false, 1, 1, TextureFormat::Bgra8UnormSrgb);",
        "let early = context\n            .device\n            .create_command_encoder(&Default::default());",
        "let early = context . queue . submit(None);",
        "let early = surface . surface . get_capabilities(&context.adapter);",
        "let early = context.instance.create_surface(self.window.clone());",
    ];
    for (method, anchor) in anchors {
        assert_eq!(renderer.matches(anchor).count(), 1, "{method} anchor");
        for seed in seeds {
            let seeded = renderer.replacen(anchor, &format!("        {seed}\n{anchor}"), 1);
            let violations = gate_violations(&[seeded.as_str()]);
            assert!(
                violations.iter().any(|violation| violation.contains(method)),
                "{method} {seed:?}: {violations:?}"
            );
        }
    }
}

/// The recovery surface helper's exact read-only operations mask nothing else: device
/// or queue work, another operation on the old surface, a `wgpu::` path beyond the
/// two allowed spellings, or a call into a GPU-reaching helper still fails the graph,
/// split or not.
#[test]
fn surface_read_exceptions_mask_no_other_gpu_work() {
    let renderer = recovery_renderer(RECOVERY_RENDERER_SOURCES);
    let anchor = "        let window = Arc::clone(&self.window);\n";
    assert_eq!(renderer.matches(anchor).count(), 1);
    let helper = "\nimpl GpuRenderer {\n    fn seeded_reaching(&self) {\n        self.queue.submit(None);\n    }\n}\n";
    for seed in [
        "self.device.create_buffer(&probe);",
        "self\n            .queue\n            .submit(None);",
        "self.surface.configure(&self.device, &self.config);",
        "self.surface /* seeded */ .get_capabilities(&self.adapter);",
        "let probe = wgpu::util::TextureBlitter::new;",
        "self.seeded_reaching();",
    ] {
        let seeded = renderer.replacen(anchor, &format!("        {seed}\n{anchor}"), 1);
        let violations = gate_violations(&[format!("{seeded}{helper}").as_str()]);
        assert!(
            violations.iter().any(|violation| violation.contains("candidate_surface")),
            "{seed:?}: {violations:?}"
        );
    }
}

/// Destroying a retired context checks the token's generation first and closes the
/// gate before the device is destroyed; nothing is polled.
#[test]
fn retired_context_destroy_closes_the_gate_first() {
    let context = code_only(include_str!("recovery_context.rs"));
    let body = block_after(&context, "pub fn destroy_retired(");
    let at = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("missing `{needle}`"));
    let check = at("retired.generation() != self.device_errors.generation()");
    let close = at("self.device_errors.request_destroy()");
    let destroy = at("self.device.destroy()");
    assert!(check < close && close < destroy);
    assert!(!body.contains(".poll("));
}

/// Assembly runs while the frame's parser guards are held, so nothing reachable from
/// `assemble_frame` creates or writes a device resource or enters the device gate; that work waits
/// until `lend` returns. A device call reintroduced into a helper assembly reaches is caught.
#[test]
fn nothing_reachable_from_assembly_touches_the_device() {
    let sources: Vec<String> =
        production_sources().into_iter().map(|(_, text)| text.replace("\r\n", "\n")).collect();
    assert_eq!(assembly_device_work(&sources), Vec::<String>::new());
    let crlf: Vec<String> = sources.iter().map(|text| text.replace('\n', "\r\n")).collect();
    assert_eq!(assembly_device_work(&crlf), Vec::<String>::new(), "CRLF reads as LF");
    let seeded: Vec<String> = sources
        .iter()
        .map(|text| {
            text.replacen(
                "        self.image_upload_rebuild_pending = true;\n",
                "        self.rebuild_image_upload_if_needed();\n",
                1,
            )
        })
        .collect();
    assert_ne!(seeded, sources, "precondition: a deferred rebuild was put back into assembly");
    assert!(assembly_device_work(&seeded)
        .iter()
        .any(|name| name == "rebuild_image_upload_if_needed"));
}
