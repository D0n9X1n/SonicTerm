//! One shared, immutable heap buffer per colour font file generation.
//!
//! A FreeType face opened on a callback stream copies every frame it extracts:
//! `FT_Stream_EnterFrame` allocates and reads when the stream has a `read`
//! callback. Most tables are read in small frames and dropped, but
//! `tt_face_load_svg` and `tt_face_load_colr` extract the whole `SVG `, `COLR`
//! and `CPAL` tables and keep them for the face's life, so each face on a colour
//! font held its own copy (18.55 MiB of `SVG ` for NotoColorEmoji). A memory
//! stream (`read: None`) makes the same call point into `base` instead.
//!
//! Only a face whose table directory lists `SVG ` or `COLR` is opened on this
//! cache's shared buffer, by FreeType and HarfBuzz alike. Every other font keeps
//! the callback stream and the per-blob read: holding every font file whole,
//! system collections such as a 75 MiB PingFang.ttc included, cost more than the
//! colour tables it saved. The directory is read with a bounded read of the
//! header and table records; a malformed or unreadable one counts as no colour
//! table.
//!
//! The cache holds only `Weak` references, so the buffer lives exactly as long
//! as some face or blob uses it. Faces are not shared; only bytes are.
//!
//! A generation is the canonical path plus length and modification time, and on
//! Unix the device and inode. Windows uses path, length and modification time:
//! its file index is not available through stable `std`. A file replaced or
//! rewritten on disk is a new generation and gets a fresh buffer; faces already
//! open keep the bytes they opened. Bytes read while the open file's generation
//! changed go to that caller alone and are never published.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};
use std::time::SystemTime;

use anyhow::Context;

use crate::locator::FontDataSource;

/// The bytes of one font file generation, shared by every stream and blob opened on it.
pub type FontBytes = Arc<Box<[u8]>>;

/// What identifies one generation of a file's contents, read from an open handle's metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Generation {
    len: u64,
    modified: Option<SystemTime>,
    file_id: Option<(u64, u64)>,
}

impl Generation {
    /// The generation `meta` describes.
    fn of(meta: &Metadata) -> Self {
        Self { len: meta.len(), modified: meta.modified().ok(), file_id: file_id(meta) }
    }
}

/// Identity of one generation of a font file: its canonical path and contents generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FileKey {
    path: PathBuf,
    generation: Generation,
}

impl FileKey {
    /// The key for `path` as `meta` describes it now.
    fn new(path: &Path, meta: &Metadata) -> Self {
        Self {
            path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            generation: Generation::of(meta),
        }
    }
}

/// Device and inode on Unix.
#[cfg(unix)]
fn file_id(meta: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

/// No stable file index elsewhere; path, length and modification time identify the file.
#[cfg(not(unix))]
fn file_id(_meta: &Metadata) -> Option<(u64, u64)> {
    None
}

/// One cache slot: its own lock serializes a miss on that file, so one buffer is published.
type Slot = Arc<Mutex<Weak<Box<[u8]>>>>;

/// Every file generation with a live buffer, or with a load in progress.
static CACHE: LazyLock<Mutex<HashMap<FileKey, Slot>>> = LazyLock::new(Default::default);

/// The shared bytes of the font file at `path`, reading it only when no face or blob holds them.
///
/// `max_len` and `library` keep each caller's size limit and error text: a file longer than
/// `max_len`, before or during the read, fails with "`<path>` is too large to pass to
/// `<library>`! (len=…)". Open, metadata and not-a-file failures carry the same context they did
/// before sharing. Concurrent misses on one file read it once: the map lock only finds or creates
/// the file's slot, and the read runs under that slot's own lock, so other files never wait for it.
pub(crate) fn shared_file_bytes(
    path: &Path,
    max_len: usize,
    library: &str,
) -> anyhow::Result<FontBytes> {
    shared_file_bytes_with(path, max_len, library, read_bounded)
}

/// Read at most `limit` bytes of `file` from its current position, appending them to `data`.
fn read_bounded(file: &mut File, data: &mut Vec<u8>, limit: u64) -> std::io::Result<usize> {
    file.by_ref().take(limit).read_to_end(data)
}

/// [`shared_file_bytes`] with the read step injected, so a test can change the file mid-read.
///
/// The generation is taken from the open handle's metadata, not a path stat, so it describes the
/// file actually read. A cached buffer is returned only while the handle still matches the key
/// after the slot lock is held; bytes are published only when the handle's generation is the same
/// before and after the read. Otherwise the bytes go to this caller alone, unshared, so no other
/// caller receives changed or mixed bytes under the key. A same-length rewrite that keeps the
/// modification time (and on Unix the inode) is indistinguishable from no change.
// Lock order: `CACHE` map guard, then released; then the per-file `slot` lock alone.
fn shared_file_bytes_with(
    path: &Path,
    max_len: usize,
    library: &str,
    read: impl FnOnce(&mut File, &mut Vec<u8>, u64) -> std::io::Result<usize>,
) -> anyhow::Result<FontBytes> {
    let mut file = File::open(path).with_context(|| format!("opening file {}", path.display()))?;
    let meta =
        file.metadata().with_context(|| format!("querying metadata for {}", path.display()))?;
    if !meta.is_file() {
        // A path that is not a regular file has no font bytes to share.
        anyhow::bail!("{} is not a file", path.display());
    }
    if usize::try_from(meta.len()).map_or(true, |len| len > max_len) {
        // A file longer than the caller's `max_len` is refused before reading or caching.
        anyhow::bail!("{} is too large to pass to {library}! (len={})", path.display(), meta.len());
    }
    let key = FileKey::new(path, &meta);
    let slot = {
        let mut map = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        prune(&mut map);
        Arc::clone(map.entry(key.clone()).or_default())
    };
    let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
    let current = |file: &File| file.metadata().ok().map(|meta| Generation::of(&meta));
    // A waiter whose file changed while it waited for the slot no longer matches the key.
    let matches_key = current(&file).as_ref() == Some(&key.generation);
    if let Some(bytes) = held.upgrade().filter(|_| matches_key) {
        // When: `held` upgrades and `matches_key`, a face or blob holds this generation; share it.
        return Ok(bytes);
    }
    // One byte past `max_len` is enough to tell that the file grew beyond it during the read.
    let limit = u64::try_from(max_len).unwrap_or(u64::MAX).saturating_add(1);
    let mut data = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    read(&mut file, &mut data, limit)
        .with_context(|| format!("reading font file {}", path.display()))?;
    if data.len() > max_len {
        // More than `max_len` bytes means the file grew during the read; refuse it as too large.
        let len = file.metadata().map_or(data.len() as u64, |meta| meta.len());
        anyhow::bail!("{} is too large to pass to {library}! (len={len})", path.display());
    }
    let unchanged = matches_key && current(&file).as_ref() == Some(&key.generation);
    let bytes: FontBytes = Arc::new(data.into_boxed_slice());
    if unchanged {
        // Only bytes read while the handle kept this key's generation are published.
        *held = Arc::downgrade(&bytes);
    }
    Ok(bytes)
}

/// Drop slots whose buffer is gone and that no load is using.
///
/// A slot is cloned only under the map lock, so a strong count of one means no thread holds it.
fn prune(map: &mut HashMap<FileKey, Slot>) {
    map.retain(|_, slot| {
        Arc::strong_count(slot) > 1 || slot.try_lock().map_or(true, |weak| weak.strong_count() > 0)
    });
}

/// Largest colour font any wrapper shares; each wrapper checks its own narrower limit too.
const ANY_LIBRARY_MAX_LEN: usize = u32::MAX as usize;

/// Tables FreeType extracts whole and keeps for the face's life (`CPAL` loads with `COLR`).
const WHOLE_COLOUR_TABLE_TAGS: [[u8; 4]; 2] = [*b"SVG ", *b"COLR"];

/// Most table records read from one face's directory; real fonts list a few dozen.
const MAX_TABLE_RECORDS: u16 = 256;

/// `sfntVersion` values of a single TrueType, CFF, Apple TrueType or Type 1 sfnt face.
const SFNT_VERSIONS: [[u8; 4]; 4] = [[0, 1, 0, 0], *b"OTTO", *b"true", *b"typ1"];

/// Whether face `face_index` of the sfnt font in `reader` lists an `SVG ` or `COLR` table.
///
/// Reads only the collection header (for a `ttcf` file), the 12-byte offset table and at most
/// [`MAX_TABLE_RECORDS`] 16-byte table records. Any read failure, an unknown `sfntVersion`, a
/// face index outside the collection or a non-zero index into a single face yields `false`.
fn face_lists_whole_colour_table(reader: &mut (impl Read + Seek), face_index: u32) -> bool {
    directory_lists_whole_colour_table(reader, face_index).unwrap_or(false)
}

/// [`face_lists_whole_colour_table`] with read failures as `Err`, so `?` keeps it flat.
fn directory_lists_whole_colour_table(
    reader: &mut (impl Read + Seek),
    face_index: u32,
) -> std::io::Result<bool> {
    let mut tag = [0u8; 4];
    reader.seek(SeekFrom::Start(0))?;
    reader.read_exact(&mut tag)?;
    let face_offset = if &tag == b"ttcf" {
        // When: `tag` is `ttcf`, find the requested face's offset table in the collection header.
        let mut header = [0u8; 8];
        reader.read_exact(&mut header)?;
        let face_count = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
        if face_index >= face_count {
            // When: `face_index` is not below `face_count`, the collection has no such face.
            return Ok(false);
        }
        let mut offset = [0u8; 4];
        reader.seek(SeekFrom::Start(12 + 4 * u64::from(face_index)))?;
        reader.read_exact(&mut offset)?;
        let face_offset = u64::from(u32::from_be_bytes(offset));
        reader.seek(SeekFrom::Start(face_offset))?;
        reader.read_exact(&mut tag)?;
        face_offset
    } else if face_index != 0 {
        // When: `face_index` is non-zero for a single-face file, FreeType would reject it too.
        return Ok(false);
    } else {
        // When: `tag` is not `ttcf` and `face_index` is 0, the directory starts the file.
        0
    };
    if !SFNT_VERSIONS.contains(&tag) {
        // When: `tag` is no known `sfntVersion`, the directory is not one this parser reads.
        return Ok(false);
    }
    let mut counts = [0u8; 8];
    reader.seek(SeekFrom::Start(face_offset + 4))?;
    reader.read_exact(&mut counts)?;
    let table_count = u16::from_be_bytes([counts[0], counts[1]]);
    if table_count > MAX_TABLE_RECORDS {
        // When: `table_count` exceeds `MAX_TABLE_RECORDS`, treat the directory as malformed.
        return Ok(false);
    }
    let mut records = vec![0u8; usize::from(table_count) * 16];
    reader.read_exact(&mut records)?;
    // Each 16-byte record starts with its 4-byte tag; `read_exact` filled whole records only.
    let (records, _) = records.as_chunks::<16>();
    Ok(records.iter().any(|record| {
        WHOLE_COLOUR_TABLE_TAGS.iter().any(|colour_tag| record[..4] == colour_tag[..])
    }))
}

/// Whether face `face_index` of the font file at `path` lists an `SVG ` or `COLR` table.
///
/// An unreadable file or malformed directory counts as `false`: the caller then opens the face
/// exactly as it would any other font, and that open reports the real error.
fn file_face_lists_whole_colour_table(path: &Path, face_index: u32) -> bool {
    File::open(path).is_ok_and(|mut file| face_lists_whole_colour_table(&mut file, face_index))
}

/// The source to open face `face_index` of `source` from: a colour face's shared bytes, else
/// `source` itself.
///
/// An on-disk face that lists an `SVG ` or `COLR` table becomes a `Memory` source over the file's
/// shared buffer, so every FreeType face and HarfBuzz blob on it shares one copy of its colour
/// tables, and anything built later from the result reads the same generation even when the file
/// is replaced or deleted on disk. `face_index` holds the face in its low 16 bits, as FreeType and
/// HarfBuzz pack it; the named-instance bits above are ignored. Every other source, every other
/// face, and a colour face whose shared read fails are returned unchanged, so they stream from
/// disk exactly as before and any error surfaces from that open.
pub(crate) fn pin_colour_face(source: &FontDataSource, face_index: u32) -> Cow<'_, FontDataSource> {
    let FontDataSource::OnDisk(path) = source else {
        // When: `source` is not `OnDisk`, its bytes are already in memory.
        return Cow::Borrowed(source);
    };
    if !file_face_lists_whole_colour_table(path, face_index & 0xFFFF) {
        // When: `file_face_lists_whole_colour_table` is false, keep streaming from disk.
        return Cow::Borrowed(source);
    }
    match shared_file_bytes(path, ANY_LIBRARY_MAX_LEN, "the font stack") {
        Ok(data) => {
            Cow::Owned(FontDataSource::Memory { name: path.to_string_lossy().into_owned(), data })
        }
        Err(err) => {
            // A failed shared read falls back to the stream, whose open reports its own error.
            log::debug!("not sharing colour font {}: {err:#}", path.display());
            Cow::Borrowed(source)
        }
    }
}

/// Test-only: the cache key `path` has now, to check later whether its slot was pruned.
#[cfg(test)]
fn test_key(path: &Path) -> FileKey {
    FileKey::new(path, &std::fs::metadata(path).expect("metadata"))
}

/// Test-only: whether the cache still has a slot for `key`.
#[cfg(test)]
fn test_has_slot(key: &FileKey) -> bool {
    CACHE.lock().unwrap_or_else(PoisonError::into_inner).contains_key(key)
}

/// Test-only: whether a live buffer is cached for `key`, without reading the file.
// Lock order: `CACHE` map guard, then the per-file `slot` lock; no production path nests them.
#[cfg(test)]
fn test_is_live(key: &FileKey) -> bool {
    let map = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    map.get(key)
        .is_some_and(|slot| slot.lock().unwrap_or_else(PoisonError::into_inner).strong_count() > 0)
}

#[cfg(test)]
#[path = "font_bytes_tests.rs"]
mod font_bytes_tests;
