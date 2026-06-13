use sonicterm_render_model::{snap_to_device_pixels, DamageRect, PixelRect};

#[test]
fn exports_geometry_helpers() {
    let rect = PixelRect { x: 1, y: 2, w: 3, h: 4 };
    assert_eq!((rect.x, rect.y, rect.w, rect.h), (1, 2, 3, 4));
    assert_eq!(snap_to_device_pixels((1.0, 2.0, 3.0, 4.0), 2.0), (1.0, 2.0, 3.0, 4.0));
}

#[test]
fn damage_rect_clips_and_unions_damage() {
    let bounds = PixelRect { x: 0, y: 0, w: 100, h: 80 };
    let mut damage = DamageRect::empty();

    damage.add_clipped(PixelRect { x: 10, y: 10, w: 20, h: 10 }, bounds);
    damage.add_clipped(PixelRect { x: 80, y: 70, w: 40, h: 20 }, bounds);
    damage.add_clipped(PixelRect { x: 200, y: 200, w: 1, h: 1 }, bounds);

    assert_eq!(damage.rect(), Some(PixelRect { x: 10, y: 10, w: 90, h: 70 }));
}
