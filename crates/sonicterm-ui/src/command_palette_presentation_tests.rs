//! Cached phrase interpolation preserves literal titles and localized order.

use super::*;
use crate::i18n::test_translator as translator;

/// Cached phrases preserve catalog word order and insert literal values without rescanning them.
#[test]
fn palette_text_slots_preserve_order_and_literal_marker_values() {
    let literal = format!("user {{name}} {TEXT_SLOT} 中文");
    for (before, after) in [("prefix ", ""), ("", " suffix"), ("prefix ", " suffix")] {
        let slot = TextSlot::new(format!("{before}{TEXT_SLOT}{after}"));
        assert_eq!(slot.render(&literal), format!("{before}{literal}{after}"));
    }
    for locale in crate::i18n::SHIPPED_LOCALES {
        let i18n = translator(locale);
        for kind in ["one", "other"] {
            let rendered = i18n
                .try_t_args(
                    "palette-command-footer",
                    Some(&[("count", TEXT_SLOT), ("count-kind", kind)]),
                )
                .unwrap();
            assert_eq!(rendered.matches(TEXT_SLOT).count(), 1);
        }
        let title = i18n.try_t_args("palette-color-title", Some(&[("title", TEXT_SLOT)])).unwrap();
        assert_eq!(title.matches(TEXT_SLOT).count(), 1);
        assert!(!title.contains('▏'), "the caret is owned by layout, not translations");
        let text = PaletteText::new(Some(&i18n));
        assert_eq!(text.color_title(&literal).matches(TEXT_SLOT).count(), 1);
        // The color title is marker-free display text; the renderer owns the caret.
        assert_eq!(text.color_title(&literal), text.color_title.render(&literal));
        assert!(!text.color_title(&literal).contains('▏'));
    }
}
