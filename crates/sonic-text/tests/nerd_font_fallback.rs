use cosmic_text::FontSystem;
use fontdb::{Family, Query, Stretch, Style, Weight};
use sonic_text::swash_rasterizer::load_bundled_fonts;

#[test]
fn bundled_jetbrainsmono_nerd_font_covers_nf_fa_font() {
    let mut fs = FontSystem::new();
    load_bundled_fonts(&mut fs);

    let families = [Family::Name("JetBrainsMono Nerd Font")];
    let query = Query {
        families: &families,
        weight: Weight::NORMAL,
        stretch: Stretch::Normal,
        style: Style::Normal,
    };
    let id = fs.db().query(&query).expect("bundled JetBrainsMono Nerd Font face");
    let font = fs.get_font(id, Weight::NORMAL).expect("font data for JetBrainsMono Nerd Font");
    assert_ne!(
        font.as_swash().charmap().map('\u{f031}'),
        0,
        "U+F031 nf-fa-font must be covered by the bundled Nerd Font face"
    );
}
