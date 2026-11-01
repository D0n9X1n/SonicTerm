//! macOS contract: FreeType's HVF driver opens and renders Apple's `hvgl`-only PingFang collection.

use super::*;

/// The reserved collection CoreText picks for Han text on macOS 15; its faces carry only `hvgl`.
const PINGFANG_UI: &str =
    "/System/Library/PrivateFrameworks/FontServices.framework/Resources/Reserved/PingFangUI.ttc";

/// First macOS release whose system `libhvf` renders; below it the driver rejects every face.
const HVF_RUNTIME_FLOOR: (u32, u32) = (15, 4);

/// Whether CI or release asked for this test to fail instead of skipping on an incapable host.
fn hvf_required() -> bool {
    std::env::var("SONICTERM_REQUIRE_HVF").as_deref() == Ok("1")
}

/// Skip with `reason`, or fail with it when `SONICTERM_REQUIRE_HVF=1`.
fn skip_unless_required(reason: &str) {
    assert!(!hvf_required(), "SONICTERM_REQUIRE_HVF=1 but {reason}");
    eprintln!("skipping the PingFang HVF render: {reason}");
}

/// Whether face 0 of the collection at `path` lists an `hvgl` table, read from its own directory
/// rather than through FreeType, so a FreeType regression cannot hide the font's capability.
fn face_zero_lists_hvgl(path: &std::path::Path) -> std::io::Result<bool> {
    let bytes = std::fs::read(path)?;
    let read_u32 = |offset: usize| -> Option<u32> {
        Some(u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
    };
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed directory");
    let face_offset = if bytes.get(..4) == Some(&b"ttcf"[..]) {
        // When: the file is a `ttcf` collection, face 0's offset table follows the header.
        read_u32(12).ok_or_else(invalid)? as usize
    } else {
        0
    };
    let table_count = bytes
        .get(face_offset + 4..face_offset + 6)
        .map(|count| u16::from_be_bytes([count[0], count[1]]))
        .ok_or_else(invalid)?;
    let records = face_offset + 12;
    Ok((0..usize::from(table_count))
        .any(|record| bytes.get(records + 16 * record..records + 16 * record + 4) == Some(b"hvgl")))
}

/// The running macOS `(major, minor)` from `sw_vers -productVersion`.
fn running_macos_version() -> Option<(u32, u32)> {
    let output = std::process::Command::new("sw_vers").arg("-productVersion").output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let mut components = text.trim().split('.').map(|component| component.parse::<u32>().ok());
    Some((components.next()??, components.next().flatten().unwrap_or(0)))
}

/// Contract: on macOS 15.4+ face 0 of `PingFangUI.ttc` opens through `ftwrap` on the shared
/// in-memory bytes, maps 中 to a glyph and renders it with ink. The `hvgl` and OS checks run
/// first and independently of FreeType; with `SONICTERM_REQUIRE_HVF=1` any unmet one fails.
#[test]
fn pingfang_face_renders_han_through_hvf() {
    let path = std::path::Path::new(PINGFANG_UI);
    if !path.is_file() {
        // When: this macOS release does not ship the reserved collection, there is nothing to open.
        return skip_unless_required(&format!("{PINGFANG_UI} is missing"));
    }
    match face_zero_lists_hvgl(path) {
        Ok(true) => {}
        other => return skip_unless_required(&format!("face 0 lists no hvgl table: {other:?}")),
    }
    let version = running_macos_version();
    if version.is_none_or(|version| version < HVF_RUNTIME_FLOOR) {
        // When: the OS predates `libhvf`'s renderer, the driver correctly refuses the face.
        return skip_unless_required(&format!("macOS {version:?} is below 15.4"));
    }
    let handle = FontDataHandle {
        source: FontDataSource::OnDisk(path.to_path_buf()),
        index: 0,
        variation: 0,
        origin: crate::locator::FontOrigin::FontDirs,
        coverage: None,
    };
    let library = Library::new().unwrap();
    let mut face = library
        .face_from_locator(&handle)
        .expect("this build's FreeType opens PingFang through its HVF driver");
    let has_read =
        // SAFETY: `face.face` is the live face this test owns, and its stream outlives it.
        unsafe { (*(*face.face).stream).read.is_some() };
    assert!(!has_read, "an hvgl face streams from the shared bytes, not a per-face copy");
    let glyph_index =
        // SAFETY: `face.face` is the live face this test owns; the lookup only reads its cmap.
        unsafe { FT_Get_Char_Index(face.face, '中' as FT_ULong) };
    assert_ne!(glyph_index, 0, "PingFang maps 中");
    face.set_font_size(16.0, 144).unwrap();
    let (load_flags, render_mode) = compute_load_flags_from_config(None, None, None, Some(144));
    let slot = face.load_and_render_glyph(glyph_index, load_flags, render_mode, false).unwrap();
    let bitmap = &slot.bitmap;
    let byte_count = bitmap.rows as usize * bitmap.pitch.unsigned_abs() as usize;
    assert!(byte_count > 0 && !bitmap.buffer.is_null(), "中 rendered an empty bitmap");
    let pixels =
        // SAFETY: FreeType's rendered bitmap holds `rows * |pitch|` bytes until the next glyph load.
        unsafe { std::slice::from_raw_parts(bitmap.buffer, byte_count) };
    assert!(pixels.iter().any(|&coverage| coverage != 0), "中 rendered no ink");
}
