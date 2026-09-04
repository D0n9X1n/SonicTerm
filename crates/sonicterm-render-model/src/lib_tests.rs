//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::{snap_to_device_pixels, DamageRect, PixelRect};

const TEST_OWNERSHIP_EXEMPTIONS: &[(&str, &str)] = &[
    ("painter", "dormant trait with no production implementation; removal is tracked separately"),
    ("pane_render", "passive frame-input structs exercised by renderer and app integration tests"),
];

/// Every direct render-model module owns a sibling suite or a stated exemption.
#[test]
fn direct_modules_declare_test_ownership() {
    // Derive modules from files so an unexported direct source cannot bypass ownership review.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut modules: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_file()))
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "rs"))
        .filter_map(|entry| entry.path().file_stem()?.to_str().map(str::to_owned))
        .filter(|module| module != "lib" && !module.ends_with("_tests"))
        .collect();
    modules.sort();

    for module in modules {
        let source = std::fs::read_to_string(root.join(format!("{module}.rs")))
            .unwrap()
            .replace("\r\n", "\n");
        let declaration =
            format!("#[cfg(test)]\n#[path = \"{module}_tests.rs\"]\nmod {module}_tests;");
        let exemption = TEST_OWNERSHIP_EXEMPTIONS.iter().find(|(name, _)| *name == module);
        assert!(
            source.contains(&declaration) || exemption.is_some(),
            "{module}.rs has no flat sibling test declaration or explicit exemption"
        );
        if source.contains(&declaration) {
            assert!(exemption.is_none(), "{module}.rs has tests and a stale exemption");
        }
    }

    for (module, reason) in TEST_OWNERSHIP_EXEMPTIONS {
        assert!(!reason.trim().is_empty(), "{module}.rs exemption needs a rationale");
        assert!(root.join(format!("{module}.rs")).is_file(), "{module}.rs exemption is stale");
    }
}

#[test]
fn exports_geometry_helpers() {
    let rect = PixelRect { x: 1, y: 2, w: 3, h: 4 };
    assert_eq!((rect.x, rect.y, rect.w, rect.h), (1, 2, 3, 4));
    assert_eq!(snap_to_device_pixels((1.0, 2.0, 3.0, 4.0), 2.0), (1.0, 2.0, 3.0, 4.0));
}

/// The legacy drawing-command trait remains implementable by compatibility callers.
#[test]
fn legacy_painter_contract_remains_implementable() {
    struct Recorder {
        quads: usize,
        text: String,
    }

    impl crate::painter::Painter for Recorder {
        fn draw_quad(&mut self, _rect: PixelRect, _color: [f32; 4]) {
            self.quads += 1;
        }

        fn draw_text(&mut self, _rect: PixelRect, text: &str, _color: [f32; 4]) {
            self.text.push_str(text);
        }
    }

    let rect = PixelRect { x: 1, y: 2, w: 3, h: 4 };
    let mut painter = Recorder { quads: 0, text: String::new() };
    crate::painter::Painter::draw_quad(&mut painter, rect, [0.0; 4]);
    crate::painter::Painter::draw_text(&mut painter, rect, "legacy", [1.0; 4]);

    assert_eq!(painter.quads, 1);
    assert_eq!(painter.text, "legacy");
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

/// The `boundary` module is the single seam `sonicterm-gpu` reaches grid/cfg/ui
/// through. These asserts prove the re-exports resolve to the *same*
/// types as the origin crates — a function typed against the origin path
/// accepts a value built through the boundary path only if they are identical.
/// If the seam ever breaks (wrong re-export, renamed module), this fails to
/// compile, catching the regression before gpu does.
#[test]
fn boundary_reexports_are_type_identical_to_origin_crates() {
    // grid: build a Grid via the boundary, hand it to code typed on the origin.
    fn takes_origin_grid(g: &sonicterm_grid::grid::Grid) -> (u16, u16) {
        (g.cols, g.rows)
    }
    let g = crate::boundary::grid::grid::Grid::new(4, 2);
    assert_eq!(takes_origin_grid(&g), (4, 2));

    // cfg: a boundary-typed Config equals an origin-typed Config default.
    fn takes_origin_cfg(c: &sonicterm_cfg::config::Config) -> bool {
        !c.theme.is_empty()
    }
    let cfg: crate::boundary::cfg::config::Config = Default::default();
    assert!(takes_origin_cfg(&cfg));

    // ui: a boundary-typed SearchState is the origin SearchState.
    fn takes_origin_search(s: &sonicterm_ui::search::SearchState) -> bool {
        s.query.is_empty()
    }
    let search = crate::boundary::ui::search::SearchState::new();
    assert!(takes_origin_search(&search));
}
