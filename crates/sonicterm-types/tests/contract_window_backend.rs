//! Contract: `WindowBackend` must be object-safe and Send.

use sonicterm_types::WindowBackend;

#[test]
fn window_backend_is_object_safe_and_send() {
    fn _accept(_: Box<dyn WindowBackend>) {}
    fn assert_send<T: Send + ?Sized>() {}
    assert_send::<dyn WindowBackend>();
}

#[test]
fn window_backend_method_shapes() {
    struct Mock;
    impl WindowBackend for Mock {
        fn inner_size_px(&self) -> (u32, u32) {
            (800, 600)
        }
        fn scale_factor(&self) -> f64 {
            2.0
        }
        fn request_redraw(&self) {}
        fn set_title(&self, _: &str) {}
    }
    let w: Box<dyn WindowBackend> = Box::new(Mock);
    assert_eq!(w.inner_size_px(), (800, 600));
    assert_eq!(w.scale_factor(), 2.0);
    w.request_redraw();
    w.set_title("test");
}
