//! Contract: `ClipboardBackend` must be object-safe and Send.

use sonicterm_types::{traits::clipboard::ClipboardError, ClipboardBackend};

#[test]
fn clipboard_backend_is_object_safe_and_send() {
    fn _accept(_: Box<dyn ClipboardBackend>) {}
    fn assert_send<T: Send + ?Sized>() {}
    assert_send::<dyn ClipboardBackend>();
}

#[test]
fn clipboard_error_is_debug() {
    let e = ClipboardError::Backend("x".into());
    let _ = format!("{e:?}");
}

#[test]
fn clipboard_backend_method_shapes() {
    struct Mock {
        buf: Option<String>,
    }
    impl ClipboardBackend for Mock {
        fn get_text(&mut self) -> Option<String> {
            self.buf.clone()
        }
        fn set_text(&mut self, t: &str) -> Result<(), ClipboardError> {
            self.buf = Some(t.to_string());
            Ok(())
        }
    }
    let mut c: Box<dyn ClipboardBackend> = Box::new(Mock { buf: None });
    assert_eq!(c.get_text(), None);
    c.set_text("hi").unwrap();
    assert_eq!(c.get_text().as_deref(), Some("hi"));
}
