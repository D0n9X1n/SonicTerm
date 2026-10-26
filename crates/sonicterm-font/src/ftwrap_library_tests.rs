//! Regression contracts for the native operations owned by this sibling.

use super::*;

/// Faces retain their creating library until native face destruction completes.
#[test]
fn faces_retain_shared_library_ownership() {
    const SOURCE: &str = concat!(
        include_str!("ftwrap.rs"),
        "\n",
        include_str!("ftwrap_names.rs"),
        "\n",
        include_str!("ftwrap_metrics.rs"),
        "\n",
        include_str!("ftwrap_library.rs")
    );

    assert!(SOURCE.contains("struct LibraryInner"));
    assert!(SOURCE.contains("library: std::rc::Rc<LibraryInner>"));
    assert!(SOURCE.contains("library: std::rc::Rc::clone(&self.inner)"));
}

/// A native face remains usable after the caller releases its library wrapper.
#[test]
fn face_remains_valid_after_creating_library_drops() {
    let handle = FontDataHandle {
        source: FontDataSource::OnDisk(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("test-fonts/rec-mono/RecMonoSt.Helens-Regular.ttf"),
        ),
        index: 0,
        variation: 0,
        origin: crate::locator::FontOrigin::FontDirs,
        coverage: None,
    };
    let face = {
        let library = Library::new().unwrap();
        library.face_from_locator(&handle).unwrap()
    };

    assert!(!face.family_name().is_empty());
    drop(face);
}

/// Disk fonts keep callback streams rather than mappings of mutable files.
#[test]
fn disk_streams_use_callback_io_and_size_proofs_name_the_real_initializer() {
    const SOURCE: &str = concat!(
        include_str!("ftwrap.rs"),
        "\n",
        include_str!("ftwrap_names.rs"),
        "\n",
        include_str!("ftwrap_metrics.rs"),
        "\n",
        include_str!("ftwrap_library.rs")
    );
    const RASTERIZER: &str = include_str!("rasterizer/freetype.rs");

    assert!(!SOURCE.contains("MmapOptions"));
    assert!(!SOURCE.contains("StreamBacking::Map"));
    assert!(!RASTERIZER.contains("face_from_locator set a character size"));
}
