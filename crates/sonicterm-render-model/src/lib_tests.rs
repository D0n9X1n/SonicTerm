//! Public-surface and direct-module test-ownership contracts for `sonicterm-render-model`.

use crate::{snap_to_device_pixels, DamageRect, PixelRect};

const TEST_OWNERSHIP_EXEMPTIONS: &[(&str, &str)] = &[
    (
        "painter",
        "source-compatibility trait with no production implementation; implementability is covered below",
    ),
    ("pane_render", "passive frame-input structs exercised by renderer and app integration tests"),
];

fn validate_test_ownership(
    root: &std::path::Path,
    exemptions: &[(&str, &str)],
) -> Result<(), String> {
    let entries: Vec<std::fs::DirEntry> = std::fs::read_dir(root)
        .map_err(|error| format!("read {}: {error}", root.display()))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("read entry under {}: {error}", root.display()))?;
    for entry in &entries {
        if entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?
            .is_dir()
        {
            return Err(format!(
                "{} is a source directory; direct modules must use flat .rs files",
                entry.file_name().to_string_lossy()
            ));
        }
    }
    let mut modules: Vec<String> = entries
        .into_iter()
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "rs"))
        .filter_map(|entry| entry.path().file_stem()?.to_str().map(str::to_owned))
        .filter(|module| module != "lib" && !module.ends_with("_tests"))
        .collect();
    modules.sort();

    for module in modules {
        let source = std::fs::read_to_string(root.join(format!("{module}.rs")))
            .map_err(|error| format!("read {module}.rs: {error}"))?
            .replace("\r\n", "\n");
        let declaration =
            format!("#[cfg(test)]\n#[path = \"{module}_tests.rs\"]\nmod {module}_tests;");
        let has_declaration = source.contains(&declaration);
        let exemption = exemptions.iter().find(|(name, _)| *name == module);
        if !has_declaration && exemption.is_none() {
            return Err(format!(
                "{module}.rs has no flat sibling test declaration or explicit exemption"
            ));
        }
        if has_declaration && exemption.is_some() {
            return Err(format!("{module}.rs has tests and a stale exemption"));
        }
        if has_declaration {
            let test_file = format!("{module}_tests.rs");
            let tests = std::fs::read_to_string(root.join(&test_file))
                .map_err(|error| format!("read {test_file}: {error}"))?
                .replace("\r\n", "\n");
            if !tests.contains("#[test]") {
                return Err(format!("{test_file} contains no behavioral tests"));
            }
        }
    }

    for (module, reason) in exemptions {
        if reason.trim().is_empty() {
            return Err(format!("{module}.rs exemption needs a rationale"));
        }
        if !root.join(format!("{module}.rs")).is_file() {
            return Err(format!("{module}.rs exemption is stale"));
        }
    }
    Ok(())
}

struct OwnershipFixture(std::path::PathBuf);

impl OwnershipFixture {
    fn new(label: &str) -> Self {
        for sequence in 0..1024 {
            let path = std::env::temp_dir().join(format!(
                "sonicterm-render-model-test-ownership-{label}-{}-{sequence}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create ownership fixture: {error}"),
            }
        }
        panic!("could not allocate ownership fixture");
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for OwnershipFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Every direct render-model module owns a sibling suite or a stated exemption.
#[test]
fn direct_modules_declare_test_ownership() {
    // Derive modules from files so an unexported direct source cannot bypass ownership review.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    validate_test_ownership(&root, TEST_OWNERSHIP_EXEMPTIONS).unwrap();
}

/// A declared but empty sibling suite does not satisfy behavioral test ownership.
#[test]
fn empty_sibling_suite_is_rejected() {
    let fixture = OwnershipFixture::new("empty-suite");
    std::fs::write(
        fixture.path().join("feature.rs"),
        "#[cfg(test)]\n#[path = \"feature_tests.rs\"]\nmod feature_tests;\n",
    )
    .unwrap();
    std::fs::write(fixture.path().join("feature_tests.rs"), "").unwrap();

    let error = validate_test_ownership(fixture.path(), &[]).unwrap_err();
    assert!(error.contains("feature_tests.rs"), "unexpected error: {error}");
}

/// A direct source-module directory fails loudly instead of bypassing the flat inventory.
#[test]
fn source_module_directory_is_rejected() {
    let fixture = OwnershipFixture::new("source-directory");
    std::fs::create_dir(fixture.path().join("nested")).unwrap();
    std::fs::write(fixture.path().join("nested/mod.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();

    let error = validate_test_ownership(fixture.path(), &[]).unwrap_err();
    assert!(error.contains("nested"), "unexpected error: {error}");
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
