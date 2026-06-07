use sonicterm_render_model::{snap_to_device_pixels, PixelRect};

#[test]
fn exports_geometry_helpers() {
    let rect = PixelRect { x: 1, y: 2, w: 3, h: 4 };
    assert_eq!((rect.x, rect.y, rect.w, rect.h), (1, 2, 3, 4));
    assert_eq!(snap_to_device_pixels((1.0, 2.0, 3.0, 4.0), 2.0), (1.0, 2.0, 3.0, 4.0));
}
