use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, SystemTime};

use super::*;
use crate::locator::{FontDataHandle, FontOrigin};

/// The small COLRv1 fixture, copied to temp files so each test owns its file generation.
const FIXTURE: &[u8] = include_bytes!("../test-fonts/colr-paint-fixture.ttf");

/// A unique temp copy of the fixture, removed when dropped.
struct TempFont(PathBuf);

impl TempFont {
    /// Write the fixture to a fresh temp path.
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "sonicterm-font-bytes-{}-{}.ttf",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, FIXTURE).expect("write temp font");
        Self(path)
    }

    /// An on-disk locator handle for this file.
    fn handle(&self) -> FontDataHandle {
        FontDataHandle {
            source: FontDataSource::OnDisk(self.0.clone()),
            index: 0,
            variation: 0,
            origin: FontOrigin::FontDirs,
            coverage: None,
        }
    }
}

// Lifecycle: dropping `TempFont` removes its temp file; a file already deleted is ignored.
impl Drop for TempFont {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The stream a FreeType face reads from: its `read` callback presence and its `base` pointer.
fn stream_of(face: &crate::ftwrap::Face) -> (bool, *const u8) {
    // SAFETY: `face.face` is a live FT_Face owned by `face`, and its stream outlives the face.
    unsafe {
        let stream = (*face.face).stream;
        ((*stream).read.is_some(), (*stream).base as *const u8)
    }
}

/// Contract: two FreeType faces opened on one on-disk file share one buffer, and each streams from
/// memory (`read` is None, `base` points into the shared bytes), so FreeType's frame extraction
/// never copies a font table per face. A second face adds no font-sized allocation: it holds the
/// same buffer (heap is not counted here; no thread-heap helper is reachable from this crate).
#[test]
fn two_faces_on_one_file_share_one_memory_buffer() {
    let font = TempFont::new();
    let lib = crate::ftwrap::Library::new().unwrap();
    let first = lib.face_from_locator(&font.handle()).unwrap();
    let second = lib.face_from_locator(&font.handle()).unwrap();
    let shared = shared_file_bytes(&font.0, usize::MAX, "test").unwrap();
    for face in [&first, &second] {
        let (has_read, base) = stream_of(face);
        assert!(!has_read, "an on-disk face must stream from memory");
        assert_eq!(base, shared.as_ptr(), "the stream points into the shared buffer");
    }
    assert_eq!(Arc::strong_count(&shared), 3, "two streams and this test hold one buffer");
}

/// Contract: the cached buffer lives exactly as long as some face or blob uses it: it stays after
/// the last face closes while a HarfBuzz blob holds it, expires after that blob drops, and the
/// empty slot is pruned by the next insert.
#[test]
fn the_buffer_expires_after_the_last_face_and_blob() {
    let font = TempFont::new();
    let key = test_key(&font.0);
    let lib = crate::ftwrap::Library::new().unwrap();
    let face = lib.face_from_locator(&font.handle()).unwrap();
    let blob = crate::hbwrap::Blob::from_source(&font.handle().source, 0).unwrap();
    assert!(test_is_live(&key));
    drop(face);
    assert!(test_is_live(&key), "the blob still holds the buffer");
    drop(blob);
    assert!(!test_is_live(&key), "the last holder dropped the buffer");
    let other = TempFont::new();
    let _held = shared_file_bytes(&other.0, usize::MAX, "test").unwrap();
    assert!(!test_has_slot(&key), "an insert prunes the expired slot");
}

/// Contract: a file whose modification time or length changed is a new generation and gets a
/// fresh buffer, while holders of the old buffer keep the bytes they opened.
#[test]
fn a_changed_file_gets_a_fresh_buffer() {
    let font = TempFont::new();
    let original = shared_file_bytes(&font.0, usize::MAX, "test").unwrap();
    let file = std::fs::File::options().write(true).open(&font.0).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(2)).unwrap();
    drop(file);
    let touched = shared_file_bytes(&font.0, usize::MAX, "test").unwrap();
    assert!(!Arc::ptr_eq(&original, &touched), "a new mtime is a new generation");
    let mut longer = FIXTURE.to_vec();
    longer.push(0);
    std::fs::write(&font.0, &longer).unwrap();
    let grown = shared_file_bytes(&font.0, usize::MAX, "test").unwrap();
    assert!(!Arc::ptr_eq(&touched, &grown), "a new length is a new generation");
    assert_eq!(grown.len(), FIXTURE.len() + 1);
    assert_eq!(&original[..], FIXTURE, "the old holder keeps the bytes it opened");
}

/// Contract: concurrent first opens of one file publish a single buffer.
#[test]
fn concurrent_misses_publish_one_buffer() {
    let font = Arc::new(TempFont::new());
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let font = Arc::clone(&font);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                shared_file_bytes(&font.0, usize::MAX, "test").unwrap()
            })
        })
        .collect();
    let buffers: Vec<FontBytes> =
        threads.into_iter().map(|thread| thread.join().unwrap()).collect();
    assert!(buffers.iter().all(|buffer| Arc::ptr_eq(buffer, &buffers[0])));
}

/// Contract: missing, directory and oversized paths fail with the same messages as before sharing.
/// A directory fails where the platform refuses it: Unix opens it and the metadata check says
/// `is not a file`, while Windows' `File::open` refuses a directory, so the open error is reported.
#[test]
fn unreadable_paths_keep_their_errors() {
    let missing = std::env::temp_dir().join("sonicterm-font-bytes-missing.ttf");
    let error = shared_file_bytes(&missing, usize::MAX, "freetype").unwrap_err();
    assert!(format!("{error:#}").starts_with("opening file "), "{error:#}");
    let error = shared_file_bytes(&std::env::temp_dir(), usize::MAX, "freetype").unwrap_err();
    if cfg!(windows) {
        assert!(format!("{error:#}").starts_with("opening file "), "{error:#}");
    } else {
        assert!(format!("{error:#}").ends_with("is not a file"), "{error:#}");
    }
    let font = TempFont::new();
    let error = shared_file_bytes(&font.0, 10, "harfbuzz").unwrap_err();
    assert!(format!("{error:#}").contains("is too large to pass to harfbuzz! (len=1368)"));
}

/// The bundled Rec Mono Regular, an ordinary outline font with no `SVG ` or `COLR` table.
fn rec_mono_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-fonts/rec-mono/RecMonoSt.Helens-Regular.ttf")
}

/// Contract: an ordinary font is never read whole. Its FreeType face opens on the callback
/// stream (`read` is Some), its HarfBuzz blob reads the file itself, and neither
/// leaves a slot in the shared-buffer cache. Sharing every font file raised memory, so only
/// colour fonts are shared.
#[test]
fn an_ordinary_font_streams_from_disk_and_leaves_no_cache_entry() {
    let path = rec_mono_path();
    let key = test_key(&path);
    let handle = FontDataHandle {
        source: FontDataSource::OnDisk(path.clone()),
        index: 0,
        variation: 0,
        origin: FontOrigin::FontDirs,
        coverage: None,
    };
    let lib = crate::ftwrap::Library::new().unwrap();
    let face = lib.face_from_locator(&handle).unwrap();
    let (has_read, _) = stream_of(&face);
    assert!(has_read, "an ordinary font must keep the callback stream");
    let _blob = crate::hbwrap::Blob::from_source(&handle.source, 0).unwrap();
    assert!(!test_has_slot(&key), "an ordinary font must not enter the shared cache");
    assert!(matches!(pin_colour_face(&handle.source, 0), Cow::Borrowed(FontDataSource::OnDisk(_))));
}

/// Builds a single-face sfnt offset table at `face_offset` listing `tags`, with zeroed records.
fn sfnt_directory(tags: &[&[u8; 4]]) -> Vec<u8> {
    let mut bytes = vec![0, 1, 0, 0];
    bytes.extend_from_slice(&u16::try_from(tags.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(&[0; 6]);
    for tag in tags {
        bytes.extend_from_slice(&tag[..]);
        bytes.extend_from_slice(&[0; 12]);
    }
    bytes
}

/// Builds a `ttcf` collection whose faces list `faces[n]` tables, directories back to back.
fn collection(faces: &[&[&[u8; 4]]]) -> Vec<u8> {
    let face_count = u32::try_from(faces.len()).unwrap();
    let mut bytes = b"ttcf".to_vec();
    bytes.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    bytes.extend_from_slice(&face_count.to_be_bytes());
    let mut directory_offset = 12 + 4 * faces.len();
    let directories: Vec<Vec<u8>> = faces.iter().map(|tags| sfnt_directory(tags)).collect();
    for directory in &directories {
        bytes.extend_from_slice(&u32::try_from(directory_offset).unwrap().to_be_bytes());
        directory_offset += directory.len();
    }
    for directory in directories {
        bytes.extend_from_slice(&directory);
    }
    bytes
}

/// Contract: the directory reader finds `SVG ` and `COLR` in a well-formed single face, and a
/// truncated, empty, oversized or unknown-version directory counts as no colour table, so such
/// a file keeps the callback stream instead of being read whole.
#[test]
fn a_malformed_directory_counts_as_no_colour_table() {
    let colr = sfnt_directory(&[b"cmap", b"COLR", b"glyf"]);
    let svg = sfnt_directory(&[b"SVG ", b"head"]);
    let plain = sfnt_directory(&[b"cmap", b"glyf", b"head"]);
    let scan = |bytes: &[u8], index| {
        face_lists_whole_colour_table(&mut std::io::Cursor::new(bytes), index)
    };
    assert!(scan(&colr, 0));
    assert!(scan(&svg, 0));
    assert!(!scan(&plain, 0));
    assert!(!scan(&colr, 1), "a single face has only index 0");
    assert!(!scan(&[], 0), "an empty file");
    assert!(!scan(&colr[..10], 0), "a truncated offset table");
    assert!(!scan(&colr[..colr.len() - 1], 0), "a truncated last table record");
    let mut unknown_version = colr.clone();
    unknown_version[..4].copy_from_slice(b"wOFF");
    assert!(!scan(&unknown_version, 0), "an unknown sfntVersion");
    let mut oversized = colr.clone();
    oversized[4..6].copy_from_slice(&(MAX_TABLE_RECORDS + 1).to_be_bytes());
    assert!(!scan(&oversized, 0), "a table count above the record bound");

    let truncated = TempFont::new();
    std::fs::write(&truncated.0, &colr[..20]).unwrap();
    assert!(matches!(
        pin_colour_face(&truncated.handle().source, 0),
        Cow::Borrowed(FontDataSource::OnDisk(_))
    ));
    assert!(!test_has_slot(&test_key(&truncated.0)), "a malformed file is never read whole");
}

/// Contract: in a `ttcf` collection the requested face's own directory decides, so a colour face
/// beside an ordinary one is shared while the ordinary face streams; named-instance bits above
/// the low 16 are ignored, and an index past the collection or a truncated header is no colour.
#[test]
fn a_collection_face_index_selects_its_own_directory() {
    let bytes = collection(&[&[b"cmap", b"glyf"], &[b"cmap", b"COLR", b"CPAL"]]);
    let scan = |bytes: &[u8], index| {
        face_lists_whole_colour_table(&mut std::io::Cursor::new(bytes), index)
    };
    assert!(!scan(&bytes, 0), "face 0 lists no colour table");
    assert!(scan(&bytes, 1), "face 1 lists COLR");
    assert!(!scan(&bytes, 2), "the collection has two faces");
    assert!(!scan(&bytes[..14], 1), "a truncated offset list");

    let font = TempFont::new();
    std::fs::write(&font.0, &bytes).unwrap();
    let source = font.handle().source;
    assert!(matches!(pin_colour_face(&source, 0), Cow::Borrowed(_)), "face 0 streams");
    assert!(matches!(pin_colour_face(&source, 1), Cow::Owned(FontDataSource::Memory { .. })));
    assert!(
        matches!(
            pin_colour_face(&source, 1 | (3 << 16)),
            Cow::Owned(FontDataSource::Memory { .. })
        ),
        "named-instance bits do not change the face"
    );
}

/// Contract: bytes read while the open file was rewritten in place with the same length are
/// returned to that caller but never published under the pre-read key, so no later or waiting
/// caller receives the mixed bytes, and the next open reads the rewritten file afresh. The read
/// step is injected: it reads half, rewrites the file with a later mtime, then reads the rest.
#[test]
fn a_same_length_rewrite_during_the_read_is_not_published() {
    let font = TempFont::new();
    let pre_read_key = test_key(&font.0);
    let rewritten: Vec<u8> = FIXTURE.iter().map(|byte| !byte).collect();
    let half = FIXTURE.len() / 2;
    let mixed = shared_file_bytes_with(&font.0, usize::MAX, "test", |file, data, limit| {
        let mut first_half = vec![0u8; half];
        file.read_exact(&mut first_half)?;
        data.extend_from_slice(&first_half);
        std::fs::write(&font.0, &rewritten)?;
        let writer = std::fs::File::options().write(true).open(&font.0)?;
        writer.set_modified(SystemTime::now() + Duration::from_secs(5))?;
        drop(writer);
        read_bounded(file, data, limit)
    })
    .unwrap();
    assert_eq!(mixed.len(), FIXTURE.len(), "the rewrite kept the length");
    assert_eq!(&mixed[..half], &FIXTURE[..half]);
    assert_eq!(&mixed[half..], &rewritten[half..], "the caller got mixed bytes");
    assert!(!test_is_live(&pre_read_key), "mixed bytes must not be published under the old key");
    let fresh = shared_file_bytes(&font.0, usize::MAX, "test").unwrap();
    assert!(!Arc::ptr_eq(&mixed, &fresh));
    assert_eq!(&fresh[..], &rewritten[..], "the next open reads the rewritten file");
}

/// Contract: a file that grows past `max_len` during the read fails with the same too-large error
/// as one that was too large before it, instead of returning more bytes than the limit allows.
/// The read step is injected: it appends to the file after the size check, then reads.
#[test]
fn a_file_that_grows_past_the_limit_during_the_read_is_too_large() {
    let font = TempFont::new();
    let max_len = FIXTURE.len() + 10;
    let error = shared_file_bytes_with(&font.0, max_len, "harfbuzz", |file, data, limit| {
        let mut appender = std::fs::File::options().append(true).open(&font.0)?;
        std::io::Write::write_all(&mut appender, &[0u8; 64])?;
        drop(appender);
        read_bounded(file, data, limit)
    })
    .unwrap_err();
    let expected_len = FIXTURE.len() + 64;
    assert!(
        format!("{error:#}")
            .contains(&format!("is too large to pass to harfbuzz! (len={expected_len})")),
        "{error:#}"
    );
    assert!(!test_is_live(&test_key(&font.0)));
}
