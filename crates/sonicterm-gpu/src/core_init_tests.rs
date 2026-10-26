//! Renderer init contracts, sharing one root fixture owner.

#[test]
fn production_instance_honours_wgpu_backend_selection() {
    // Protect deterministic CI and user diagnostics from an ignored `WGPU_BACKEND` override.
    const CORE_SRC: &str = crate::core::CORE_SOURCE;
    assert!(CORE_SRC.contains("InstanceDescriptor::new_with_display_handle_from_env"));
    assert!(!CORE_SRC.contains("InstanceDescriptor::new_with_display_handle(Box::new"));
}
