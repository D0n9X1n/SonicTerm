//! Contract: `Painter` must be object-safe and Send.

use sonicterm_types::{FrameLike, PaintError, Painter};

#[test]
fn painter_is_object_safe_and_send() {
    fn _accept(_: Box<dyn Painter>) {}
    fn assert_send<T: Send + ?Sized>() {}
    assert_send::<dyn Painter>();
}

#[test]
fn paint_error_is_debug() {
    let e = PaintError::Other("x".into());
    let _ = format!("{e:?}");
}

#[test]
fn painter_method_shapes() {
    struct FrameMock;
    impl FrameLike for FrameMock {
        fn cols(&self) -> u32 {
            80
        }
        fn rows(&self) -> u32 {
            24
        }
    }
    struct PaintMock;
    impl Painter for PaintMock {
        fn paint_frame(&mut self, frame: &dyn FrameLike) -> Result<(), PaintError> {
            assert_eq!(frame.cols(), 80);
            assert_eq!(frame.rows(), 24);
            Ok(())
        }
        fn resize_surface(&mut self, _w: u32, _h: u32) {}
    }
    let mut p: Box<dyn Painter> = Box::new(PaintMock);
    p.paint_frame(&FrameMock).unwrap();
    p.resize_surface(100, 100);
}
