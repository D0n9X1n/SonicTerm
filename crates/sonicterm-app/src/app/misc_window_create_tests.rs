//! New terminal windows preserve dimensions and reuse the live GPU device.

/// New-window requests retain their initiating dimensions instead of choosing a fixed destination at drain time.
#[test]
fn new_window_constructor_uses_requested_dimensions() {
    let source = MISC_SOURCES;
    let start = source.find("fn create_new_terminal_window(").unwrap();
    let constructor = &source[start..];
    assert!(constructor.contains(".with_inner_size(request.inner_size)"));
    assert!(!constructor.contains("LogicalSize::new(800.0, 500.0)"));
}

/// New Window shares the live GPU device instead of always opening a second one.
#[test]
fn new_window_constructor_reuses_the_live_gpu_device() {
    // Every host compiles this check; the Windows-native tests below prove the device identity.
    let source = MISC_SOURCES;
    let start = source.find("fn create_new_terminal_window(").unwrap();
    let body: Vec<&str> = source[start..].lines().take_while(|line| *line != "    }").collect();
    let body = body.join("\n");
    assert!(body.contains("let shared_gpu = self.shared_gpu_context();"));
    assert!(body.contains("GpuRenderer::new_with_shared_context("));
    assert!(!body.contains("match GpuRenderer::new("));
}

/// Complete logical owner sources, including the flat extraction modules.
const MISC_SOURCES: &str = concat!(
    include_str!("misc.rs"),
    "\n",
    include_str!("misc_clipboard.rs"),
    "\n",
    include_str!("misc_window_create.rs")
);
