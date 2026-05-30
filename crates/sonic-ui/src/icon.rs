use tiny_skia::{Pixmap, Transform};
use usvg::{Options, Tree};

use crate::prefs::Category;

/// Embedded SVG icon metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icon {
    pub key: &'static str,
    pub svg: &'static str,
}

pub const FONT: Icon = Icon { key: "font", svg: include_str!("../../../assets/icons/ui/font.svg") };
pub const THEME: Icon =
    Icon { key: "theme", svg: include_str!("../../../assets/icons/ui/theme.svg") };
pub const KEYMAP: Icon =
    Icon { key: "keymap", svg: include_str!("../../../assets/icons/ui/keymap.svg") };
pub const WINDOW: Icon =
    Icon { key: "window", svg: include_str!("../../../assets/icons/ui/window.svg") };
pub const CURSOR: Icon =
    Icon { key: "cursor", svg: include_str!("../../../assets/icons/ui/cursor.svg") };
pub const ADVANCED: Icon =
    Icon { key: "advanced", svg: include_str!("../../../assets/icons/ui/advanced.svg") };

// Chrome icons are app UI, not terminal cell content. Keep them in SVG form so
// window/tab chrome never depends on Nerd Font or other font fallback coverage.
pub const CLOSE: Icon =
    Icon { key: "close", svg: include_str!("../../../assets/icons/ui/close.svg") };
pub const PLUS: Icon = Icon { key: "plus", svg: include_str!("../../../assets/icons/ui/plus.svg") };
pub const MINIMIZE: Icon =
    Icon { key: "minimize", svg: include_str!("../../../assets/icons/ui/minimize.svg") };
pub const MAXIMIZE: Icon =
    Icon { key: "maximize", svg: include_str!("../../../assets/icons/ui/maximize.svg") };

pub const ALL: &[Icon] =
    &[FONT, THEME, KEYMAP, WINDOW, CURSOR, ADVANCED, CLOSE, PLUS, MINIMIZE, MAXIMIZE];

pub fn for_category(cat: Category) -> &'static Icon {
    match cat {
        Category::Font => &FONT,
        Category::Theme => &THEME,
        Category::Keymap => &KEYMAP,
        Category::Window => &WINDOW,
        Category::Cursor => &CURSOR,
        Category::Advanced => &ADVANCED,
    }
}

/// Rasterize a bundled SVG icon into an alpha mask.
///
/// Parsing and drawing go through usvg/resvg so standard SVG features such as
/// stroke caps/joins, transforms, viewBox scaling, and curves are honored. The
/// GPU render path tints the returned alpha mask with theme colors at draw time.
pub fn rasterize_alpha(icon: &Icon, size_px: u32) -> Vec<u8> {
    let Some(mut pixmap) = Pixmap::new(size_px, size_px) else {
        return Vec::new();
    };

    if let Ok(tree) = Tree::from_str(icon.svg, &Options::default()) {
        let svg_size = tree.size();
        let scale_x = size_px as f32 / svg_size.width();
        let scale_y = size_px as f32 / svg_size.height();
        resvg::render(&tree, Transform::from_scale(scale_x, scale_y), &mut pixmap.as_mut());
    }

    pixmap.pixels().iter().map(|px| px.alpha()).collect()
}
