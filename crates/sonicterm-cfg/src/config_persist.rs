//! Atomic, format-preserving persistence of runtime font and status settings.

use super::*;
use anyhow::bail;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use toml_edit::{DocumentMut, Item, Table, TableLike};

static FONT_PERSIST_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static FONT_PERSIST_PROCESS_PATHS: std::sync::Mutex<Vec<PathBuf>> =
    std::sync::Mutex::new(Vec::new());

impl Config {
    /// Persist runtime font size and weight without rewriting unrelated TOML.
    ///
    /// This validates and reparses the current disk document before atomically
    /// replacing it; comments, unknown keys, ordering, and scalar decoration are
    /// preserved. The replacement does not claim directory-fsync durability.
    pub fn persist_font_runtime_values(path: &Path, size: f32, weight_scale: f32) -> Result<()> {
        Self::persist_font_runtime_values_before_commit(path, size, weight_scale, |_| Ok(()))
    }

    pub(super) fn persist_font_runtime_values_before_commit<F>(
        path: &Path,
        size: f32,
        weight_scale: f32,
        before_commit: F,
    ) -> Result<()>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        Self::persist_runtime_values_before_commit(
            path,
            size,
            weight_scale,
            None,
            None,
            before_commit,
        )
        .map(|_| ())
    }
    /// Persist the runtime font values and, with `status_line`, the session's `[status_line]`
    /// state, without rewriting unrelated TOML.
    ///
    /// Under the save lock it reads and strictly parses the file on disk, resolves its
    /// `[status_line]`, and writes only the keys whose value differs from the session: `enabled`,
    /// `hide_sensitive`, `position`, `font_scale`, and each of `left`, `center` and `right` as an
    /// explicit list. A key in the file is updated in place, even to its default; an absent key is
    /// written only when the session differs from its default. `preset` and
    /// per-segment keys are never written. The rendered document must parse and resolve to exactly
    /// the session's values before it replaces the file. With `family`, `[font] family` follows
    /// the same rule against the family the session started with. Returns the written
    /// `[status_line]`, resolved, as the new baseline.
    pub fn persist_runtime_values(
        path: &Path,
        size: f32,
        weight_scale: f32,
        family: Option<FamilySave<'_>>,
        status_line: Option<&crate::status_overrides::StatusLineSave>,
    ) -> Result<crate::status_overrides::WrittenStatusLine> {
        Self::persist_runtime_values_before_commit(
            path,
            size,
            weight_scale,
            family,
            status_line,
            |_| Ok(()),
        )
    }

    pub(super) fn persist_runtime_values_before_commit<F>(
        path: &Path,
        size: f32,
        weight_scale: f32,
        family: Option<FamilySave<'_>>,
        status_line: Option<&crate::status_overrides::StatusLineSave>,
        before_commit: F,
    ) -> Result<crate::status_overrides::WrittenStatusLine>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        validate_font_runtime_values(size, weight_scale)?;
        if !path.exists() {
            Self::ensure_user_config_file(path)?;
        }

        let destination = canonical_destination(path)?;
        let _save_lock = ConfigSaveLock::acquire(&destination)?;
        let bytes = std::fs::read(&destination)
            .with_context(|| format!("read config for font persistence at {destination:?}"))?;
        let text = std::str::from_utf8(&bytes)
            .with_context(|| format!("decode config for font persistence at {destination:?}"))?;
        let line_ending = line_ending_convention(text)?;
        let on_disk: Self = toml::from_str(text)
            .with_context(|| format!("strictly parse config at {destination:?}"))?;
        let normalized = normalize_for_toml_edit(text, line_ending);
        let mut document: DocumentMut = normalized
            .parse()
            .with_context(|| format!("format-preserving parse config at {destination:?}"))?;

        patch_font_runtime_values(&mut document, size, weight_scale)?;
        if let Some(family) = family {
            patch_font_family(&mut document, family)?;
        }
        if let Some(save) = status_line {
            let disk = on_disk
                .status_line
                .resolve()
                .with_context(|| format!("resolve [status_line] of config at {destination:?}"))?;
            patch_status_line_values(&mut document, save, &disk)?;
        }
        let rendered = apply_line_ending(document.to_string(), line_ending);
        verify_persisted_font_runtime_values(&rendered, size, weight_scale)?;
        let written = verify_persisted_status_line(&rendered, status_line)?;
        atomic_replace_if_unchanged(&destination, &bytes, rendered.as_bytes(), before_commit)?;
        Ok(written)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LineEnding {
    Lf,
    CrLf,
}

fn validate_font_runtime_values(size: f32, weight_scale: f32) -> Result<()> {
    if !size.is_finite() || size <= 0.0 {
        bail!("font size must be finite and greater than zero");
    }
    if !weight_scale.is_finite() || !(0.5..=5.0).contains(&weight_scale) {
        bail!("font weight scale must be finite and in 0.5..=5.0");
    }
    Ok(())
}

fn canonical_destination(path: &Path) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect config destination {path:?}"))?;
    if metadata.file_type().is_symlink() {
        std::fs::canonicalize(path).with_context(|| format!("resolve config symlink {path:?}"))
    } else {
        // When: `!metadata.file_type().is_symlink()`, replace the regular destination path directly.
        Ok(path.to_path_buf())
    }
}

pub(super) fn line_ending_convention(text: &str) -> Result<LineEnding> {
    let bytes = text.as_bytes();
    let mut saw_lf = false;
    let mut saw_crlf = false;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                if bytes.get(index + 1) != Some(&b'\n') {
                    bail!("config contains a bare carriage return");
                }
                saw_crlf = true;
                index += 2;
            }
            b'\n' => {
                saw_lf = true;
                index += 1;
            }
            _ => index += 1,
        }
    }
    if saw_lf && saw_crlf {
        bail!("config mixes LF and CRLF line endings");
    }
    Ok(if saw_crlf { LineEnding::CrLf } else { LineEnding::Lf })
}

pub(super) fn normalize_for_toml_edit(
    text: &str,
    line_ending: LineEnding,
) -> std::borrow::Cow<'_, str> {
    if matches!(line_ending, LineEnding::CrLf) {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        // When: matches!(line_ending, LineEnding::CrLf) is false, toml_edit can parse the original LF bytes directly.
        std::borrow::Cow::Borrowed(text)
    }
}

pub(super) fn apply_line_ending(mut rendered: String, line_ending: LineEnding) -> String {
    if matches!(line_ending, LineEnding::CrLf) {
        rendered = rendered.replace('\n', "\r\n");
    }
    rendered
}

/// The session's font family for Save, beside the family the session started with.
///
/// Save writes `[font] family` when the file already names one, updating it in place, or when
/// `live` differs from `baseline`; an absent key whose value never changed stays absent.
#[derive(Debug, Clone, Copy)]
pub struct FamilySave<'a> {
    /// The live family.
    pub live: &'a str,
    /// The family the session started with.
    pub baseline: &'a str,
}

/// Write `[font] family` by the Save rule: a key in the file is updated in place, keeping its
/// decor; an absent key is written only when the live family differs from the starting one.
fn patch_font_family(document: &mut DocumentMut, family: FamilySave<'_>) -> Result<()> {
    let present = document
        .get("font")
        .and_then(Item::as_table_like)
        .is_some_and(|font| font.contains_key("family"));
    if !present && family.live == family.baseline {
        // When: the key is absent and the family never changed, Save adds nothing.
        return Ok(());
    }
    if document.get("font").is_none() {
        document.insert("font", Item::Table(Table::new()));
    }
    let font = document
        .get_mut("font")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| anyhow::anyhow!("font must be a table or inline table"))?;
    let mut replacement = toml_edit::value(family.live);
    if let Some(existing) = font.get_mut("family") {
        // The file names `family`: its value changes in place and keeps its comments.
        if let (Some(old), Some(new)) = (existing.as_value(), replacement.as_value_mut()) {
            *new.decor_mut() = old.decor().clone();
        }
        *existing = replacement;
    } else {
        // When: `font.get_mut("family")` is None, the family changed, so only that key is inserted.
        font.insert("family", replacement);
    }
    Ok(())
}

fn patch_font_runtime_values(
    document: &mut DocumentMut,
    size: f32,
    weight_scale: f32,
) -> Result<()> {
    if document.get("font").is_none() {
        document.insert("font", Item::Table(Table::new()));
    }
    let font = document
        .get_mut("font")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| anyhow::anyhow!("font must be a table or inline table"))?;
    patch_numeric_value(font, "size", size)?;
    patch_numeric_value(font, "weight_scale", weight_scale)
}

fn patch_numeric_value(table: &mut dyn TableLike, key: &str, value: f32) -> Result<()> {
    let mut replacement =
        value.to_string().parse::<Item>().with_context(|| format!("format font {key}"))?;
    if let Some(existing) = table.get_mut(key) {
        if !existing.is_integer() && !existing.is_float() {
            bail!("font.{key} must be an integer or float");
        }
        let existing_decor = existing.as_value().expect("numeric items are values").decor().clone();
        *replacement.as_value_mut().expect("parsed numeric item is a value").decor_mut() =
            existing_decor;
        *existing = replacement;
    } else {
        // When: the font key is absent, insert only that scalar without rebuilding the surrounding table.
        table.insert(key, replacement);
    }
    Ok(())
}

/// Write each `[status_line]` key whose session value differs from `disk`, creating the table
/// only when a key is written; every other key, comment and table stays. Because an absent key
/// resolves to its default, this is the Save rule: a key in the file is updated in place, even to
/// its default, and an absent key is written only when the session differs from its default.
/// Check the rendered document loads, and that its `[status_line]` resolves to exactly `save`.
fn verify_persisted_status_line(
    rendered: &str,
    save: Option<&crate::status_overrides::StatusLineSave>,
) -> Result<crate::status_overrides::WrittenStatusLine> {
    let config: Config = toml::from_str(rendered).context("reparse rendered config")?;
    config.status_line.validate().context("the rendered [status_line] would not load")?;
    let written = config.status_line.resolve().context("resolve rendered [status_line]")?;
    if let Some(save) = save {
        if crate::status_overrides::StatusLineSave::from_resolved(&written) != *save {
            // The written lists resolve to something other than `save`, so the save would not hold.
            bail!("rendered status line values do not match the session");
        }
    }
    Ok(crate::status_overrides::WrittenStatusLine { table: config.status_line, resolved: written })
}

fn verify_persisted_font_runtime_values(
    rendered: &str,
    size: f32,
    weight_scale: f32,
) -> Result<()> {
    let config: Config = toml::from_str(rendered).context("reparse rendered config")?;
    if config.font.size != size || config.font.weight_scale != weight_scale {
        bail!("rendered font values do not match the requested runtime values");
    }
    Ok(())
}

struct ProcessSavePathGuard {
    path: PathBuf,
}

impl ProcessSavePathGuard {
    fn acquire(destination: &Path) -> Result<Self> {
        let mut paths = FONT_PERSIST_PROCESS_PATHS
            .lock()
            .map_err(|_| anyhow::anyhow!("config save path registry is poisoned"))?;
        // A duplicate path means one window already owns this config's save slot.
        if paths.iter().any(|path| path == destination) {
            bail!("another SonicTerm window is saving the config; retry the save");
        }
        paths.push(destination.to_path_buf());
        Ok(Self { path: destination.to_path_buf() })
    }
}

// Lifecycle: ProcessSavePathGuard drop removes its canonical destination from the in-process save registry.
impl Drop for ProcessSavePathGuard {
    fn drop(&mut self) {
        // A poisoned registry cannot be repaired safely during unwinding; the
        // process is already failing, so cleanup remains best-effort.
        if let Ok(mut paths) = FONT_PERSIST_PROCESS_PATHS.lock() {
            paths.retain(|path| path != &self.path);
        }
    }
}

struct ConfigSaveLock {
    _process: ProcessSavePathGuard,
    file: File,
}

impl ConfigSaveLock {
    fn acquire(destination: &Path) -> Result<Self> {
        // OS advisory locks may not conflict with another descriptor in this
        // process, so a path registry covers sibling windows as well.
        let process = ProcessSavePathGuard::acquire(destination)?;
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        let mut name = destination
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("sonicterm.toml"))
            .to_os_string();
        name.push(".save.lock");
        let path = parent.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open config save lock {path:?}"))?;
        let lock_result = file.try_lock();
        if matches!(lock_result, Err(std::fs::TryLockError::WouldBlock)) {
            // A held sidecar means another process owns this config's persistent save slot.
            bail!("another SonicTerm instance is saving the config; retry the save");
        }
        if let Err(std::fs::TryLockError::Error(error)) = lock_result {
            // When: `lock_result` carries an OS `Error`, preserve that context instead of calling it contention.
            return Err(error).with_context(|| format!("lock config save path {path:?}"));
        }
        Ok(Self { _process: process, file })
    }
}

// Lifecycle: ConfigSaveLock drop releases the advisory lock; the sidecar stays so every process locks the same inode.
impl Drop for ConfigSaveLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

struct TempFileGuard {
    path: PathBuf,
    file: Option<File>,
}

impl TempFileGuard {
    // Ordering: FONT_PERSIST_TEMP_SEQUENCE uses Relaxed because create_new enforces uniqueness without inter-thread state publication.
    fn create(destination: &Path) -> Result<Self> {
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        let file_name =
            destination.file_name().unwrap_or_else(|| std::ffi::OsStr::new("sonicterm.toml"));
        loop {
            let sequence = FONT_PERSIST_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let mut temp_name = file_name.to_os_string();
            temp_name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
            let path = parent.join(temp_name);
            // When: create_new finds an existing PID/sequence temp, advance without truncating that file.
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok(Self { path, file: Some(file) }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| format!("create temporary config {path:?}"));
                }
            }
        }
    }
}

// Lifecycle: TempFileGuard drop releases its uncommitted temp path; successful rename consumes that path before release.
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.file.take().is_some() || self.path.exists() {
            // When: `self.file` was open or `self.path.exists()`, remove the staged temp best-effort.
            #[cfg(windows)]
            #[allow(clippy::permissions_set_readonly_false)]
            if let Ok(metadata) = std::fs::metadata(&self.path) {
                // When: metadata is readable, clear only the staged file's read-only bit before deletion.
                let mut permissions = metadata.permissions();
                if permissions.readonly() {
                    // When: permissions.readonly() is true on the staged temp,
                    // clear it so a failed replacement cannot strand that file.
                    permissions.set_readonly(false);
                    let _ = std::fs::set_permissions(&self.path, permissions);
                }
            }
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn atomic_replace_if_unchanged<F>(
    destination: &Path,
    expected: &[u8],
    bytes: &[u8],
    before_commit: F,
) -> Result<()>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let permissions = std::fs::metadata(destination)
        .with_context(|| format!("read config permissions at {destination:?}"))?
        .permissions();
    let mut temp = TempFileGuard::create(destination)?;
    let file = temp.file.as_mut().expect("temporary file remains open before replacement");
    file.write_all(bytes).with_context(|| format!("write temporary config {:?}", temp.path))?;
    file.sync_all().with_context(|| format!("sync temporary config {:?}", temp.path))?;
    std::fs::set_permissions(&temp.path, permissions)
        .with_context(|| format!("copy config permissions to {:?}", temp.path))?;
    temp.file.take();

    before_commit(destination)?;
    // The sidecar serializes SonicTerm writers; this final byte comparison also
    // rejects an external editor change observed after parsing.
    let current = std::fs::read(destination)
        .with_context(|| format!("re-read config before replacement at {destination:?}"))?;
    if current != expected {
        bail!("config changed while current settings were being saved; retry the save");
    }

    replace_file(&temp.path, destination)
        .with_context(|| format!("replace config {:?} -> {destination:?}", temp.path))
}

fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(not(windows))]
    {
        std::fs::rename(source, destination)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_WRITE_THROUGH};

        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: both UTF-16 paths are NUL-terminated and live through the call; ReplaceFileW preserves destination metadata.
        unsafe {
            ReplaceFileW(
                PCWSTR(destination.as_ptr()),
                PCWSTR(source.as_ptr()),
                PCWSTR::null(),
                REPLACEFILE_WRITE_THROUGH,
                None,
                None,
            )
        }
        .map_err(std::io::Error::other)
    }
}

#[cfg(test)]
#[path = "config_persist_tests.rs"]
mod config_persist_tests;
