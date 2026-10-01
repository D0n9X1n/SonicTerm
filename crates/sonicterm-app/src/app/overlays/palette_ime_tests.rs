use sonicterm_gpu::field_geometry::FieldRect;
use winit::dpi::{PhysicalPosition, PhysicalSize};

use super::{field_ime_anchor_for, field_ime_area, FieldImeAnchor};

/// A presented caret rectangle converts to whole physical pixels: the origin
/// rounds, the extent rounds up, and a hairline caret keeps at least one pixel.
#[test]
fn field_ime_area_maps_a_caret_to_physical_pixels() {
    let caret = FieldRect { x: 120.4, y: 30.6, w: 0.3, h: 17.2 };
    let (position, size) = field_ime_area(caret);
    assert_eq!(position, PhysicalPosition::new(120, 31));
    assert_eq!(size, PhysicalSize::new(1, 18));
}

/// Field ownership decides the anchor: a presented caret anchors the candidate
/// box, a field without a presented caret suppresses the terminal fallback, and
/// only an unowned window falls back to the terminal cursor.
#[test]
fn field_ownership_selects_the_ime_anchor() {
    let caret = FieldRect { x: 10.0, y: 20.0, w: 2.0, h: 16.0 };
    assert_eq!(field_ime_anchor_for(Some(Some(caret))), FieldImeAnchor::Field(caret));
    assert_eq!(
        field_ime_anchor_for(Some(None)),
        FieldImeAnchor::Pending,
        "a field-owned window never borrows the terminal anchor while its caret is unpresented"
    );
    assert_eq!(field_ime_anchor_for(None), FieldImeAnchor::Terminal);
}
