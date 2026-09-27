use std::rc::Rc;

use super::*;

/// The text of `recovery_context.rs`'s `fn name`, from its signature to its closing brace.
fn method(name: &str) -> String {
    let source = include_str!("recovery_context.rs").replace("\r\n", "\n");
    let header = format!("fn {name}(");
    let start = source.find(&header).unwrap_or_else(|| panic!("missing `{name}`"));
    let end = source[start..].find("\n    }\n").unwrap_or_else(|| panic!("unterminated `{name}`"));
    source[start..start + end].to_owned()
}

/// On macOS the recovery surface clones the window's own Metal layer into a local
/// binding and drops the layer mutex guard inside the `as_hal` closure, and it
/// refuses a surface that is not Metal-backed instead of wrapping the view again; the
/// plain window surface is built only off macOS, and every candidate keeps its instance.
#[test]
fn candidate_surface_reuses_the_metal_layer_or_refuses() {
    let body = method("candidate_surface");
    let at = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("missing `{needle}`"));
    let lock = at("let guard = hal.render_layer().lock();");
    let retained = at("let retained = guard.clone();");
    let release = at("drop(guard);");
    let refuse = at("return Err(anyhow!(\"recovery requires the window's Metal layer\"))");
    let wrap = at("wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(raw)");
    assert!(lock < retained && retained < release && release < refuse && refuse < wrap);
    let plain = at("#[cfg(not(target_os = \"macos\"))]");
    assert_eq!(body.matches("instance.create_surface(").count(), 1);
    assert!(wrap < plain && plain < at("instance.create_surface("), "view surfaces only off macOS");
    assert!(body.contains("instance: instance.clone()"));
}

/// A failed negotiation hands the owned value back with its error instead of
/// dropping it: the value is released only when the caller drops it. A successful
/// negotiation returns it too.
#[test]
fn failed_negotiation_hands_the_owned_value_back_undropped() {
    let witness = Rc::new(());
    let failed = keep_on_failure(Rc::clone(&witness), |_| -> anyhow::Result<()> {
        Err(anyhow::anyhow!("no suitable GPU adapter"))
    });
    let (error, owned) = failed.expect_err("negotiation fails");
    assert_eq!(error.to_string(), "no suitable GPU adapter");
    assert_eq!(Rc::strong_count(&witness), 2, "the failure still owns the value");
    drop(owned);
    assert_eq!(Rc::strong_count(&witness), 1, "only the caller's drop releases it");
    let succeeded = keep_on_failure(Rc::clone(&witness), |_| Ok(7));
    let (value, owned) = succeeded.expect("negotiation succeeds");
    assert_eq!((value, Rc::strong_count(&witness)), (7, 2));
    drop(owned);
    assert_eq!(Rc::strong_count(&witness), 1);
}

/// A failed request keeps its surface for the caller: `run` routes negotiation
/// through `keep_on_failure` into a `RequestFailure` instead of dropping the surface
/// on the worker, installs the waker only after success, and the failure's `Debug`
/// writes only its error. It is no `std::error::Error`, so `?` cannot drop it.
#[test]
fn request_failure_keeps_the_surface_and_formats_only_the_error() {
    let run = method("run");
    let at = |needle: &str| run.find(needle).unwrap_or_else(|| panic!("missing `{needle}`"));
    let keep = at("keep_on_failure(self.surface,");
    let fail = at("RequestFailure { error, surface: Box::new(surface) }");
    let waker = at("set_waker(make_waker(generation))");
    assert!(keep < fail && fail < waker);
    assert!(!run.contains("negotiate_device(&self.surface"));
    let debug = method("fmt");
    assert!(debug.contains(".field(\"error\", &self.error)") && !debug.contains("self.surface"));
    let source = include_str!("recovery_context.rs");
    assert!(source.contains("pub fn into_parts(self) -> (anyhow::Error, CandidateSurface) {"));
    assert!(!source.contains("impl std::error::Error for RequestFailure"));
}
