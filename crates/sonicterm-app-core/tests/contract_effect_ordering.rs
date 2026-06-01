//! Contract test: `EffectClass` ordering matches spec §6 table.
//!
//! For every pair (a, b) where class(a) < class(b), build `[b, a]`,
//! sort by `effect_class`, and assert the stable sort produced
//! `[a, b]`. Also covers the intra-class stability guarantee.

use sonicterm_app_core::{AppEffect, EffectClass, LogLevel, MenuModel, PaneId, RedrawReason};
use sonicterm_types::WindowKey;

fn representative(class: EffectClass) -> AppEffect {
    match class {
        EffectClass::PtyWrite => AppEffect::PtyClose { pane: PaneId(1) },
        EffectClass::Render => {
            AppEffect::Render { window: WindowKey::new(1), reason: RedrawReason::Vsync }
        }
        EffectClass::OsDrag => {
            AppEffect::OsDragEnd { src_window: WindowKey::new(1), committed: true }
        }
        EffectClass::Clipboard => AppEffect::ClipboardSet { text: "x".into() },
        EffectClass::WindowOp => AppEffect::Quit,
        EffectClass::MenubarUpdate => AppEffect::MenubarUpdate(MenuModel::default()),
        EffectClass::Log => {
            AppEffect::LogEvent { level: LogLevel::Info, target: "t", msg: "m".into() }
        }
    }
}

const ALL_CLASSES: &[EffectClass] = &[
    EffectClass::PtyWrite,
    EffectClass::Render,
    EffectClass::OsDrag,
    EffectClass::Clipboard,
    EffectClass::WindowOp,
    EffectClass::MenubarUpdate,
    EffectClass::Log,
];

#[test]
fn ordering_matches_spec_table_for_every_pair() {
    for &a in ALL_CLASSES {
        for &b in ALL_CLASSES {
            if (a as u8) >= (b as u8) {
                continue;
            }
            let mut v = [representative(b), representative(a)];
            v.sort_by_key(AppEffect::effect_class);
            assert_eq!(v[0].effect_class(), a, "expected {a:?} before {b:?}");
            assert_eq!(v[1].effect_class(), b);
        }
    }
}

#[test]
fn sort_is_stable_within_class() {
    // Two PtyWrites pushed in order (a, b) must come out (a, b),
    // never swapped — critical for modifier+char keystrokes.
    let a = AppEffect::PtyClose { pane: PaneId(1) };
    let b = AppEffect::PtyClose { pane: PaneId(2) };
    let mut v = [a, b];
    v.sort_by_key(AppEffect::effect_class);
    match (&v[0], &v[1]) {
        (AppEffect::PtyClose { pane: p0 }, AppEffect::PtyClose { pane: p1 }) => {
            assert_eq!(p0, &PaneId(1));
            assert_eq!(p1, &PaneId(2));
        }
        _ => panic!("expected two PtyClose variants"),
    }
}

#[test]
fn pty_then_render_sorts_pty_first() {
    let render = AppEffect::Render { window: WindowKey::new(7), reason: RedrawReason::PtyBytes };
    let pty = AppEffect::PtyClose { pane: PaneId(9) };
    let mut v = [render, pty];
    v.sort_by_key(AppEffect::effect_class);
    assert!(matches!(v[0], AppEffect::PtyClose { .. }));
    assert!(matches!(v[1], AppEffect::Render { .. }));
}

#[test]
fn effect_class_values_are_zero_to_six() {
    for &c in ALL_CLASSES {
        let v = c as u8;
        assert!(v <= 6, "EffectClass {c:?} should be 0..=6 (spec §6); got {v}");
    }
}
