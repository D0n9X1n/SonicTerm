//! Contract: `FrameLike` describes the type-erased frame shape consumed
//! by `Painter`. Must be object-safe (Painter takes `&dyn FrameLike`).

use sonicterm_types::FrameLike;

#[test]
fn frame_like_is_object_safe() {
    fn _accept(_: &dyn FrameLike) {}
}

#[test]
fn frame_like_method_shapes() {
    struct Mock;
    impl FrameLike for Mock {
        fn cols(&self) -> u32 {
            80
        }
        fn rows(&self) -> u32 {
            24
        }
    }
    let m: &dyn FrameLike = &Mock;
    assert_eq!(m.cols(), 80);
    assert_eq!(m.rows(), 24);
}
