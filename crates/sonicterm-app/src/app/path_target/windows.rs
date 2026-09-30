//! Windows path probes and direct-open. Classification walks a drive-absolute path from its root
//! and follows a symlink or junction only when the drive holding it and the drive its target names
//! are both local fixed drives, so following a link never reaches a UNC host or a mapped network
//! drive. Other reparse points, and filenames with alternate-stream or normalization-sensitive
//! syntax, stay blocked. Files are selected in Explorer and directories open through
//! `ShellExecuteExW`. The module is declared as `windows_os` so the `windows::` paths here keep
//! naming the `windows` crate.

use super::*;

/// Most symlinks and junctions one classification follows. Windows resolves at most 31 reparse
/// points on a path whose links name fully qualified targets, so a longer chain, or a loop, is refused.
const MAX_LINK_HOPS: usize = 31;

/// Most entries one classification reads, bounding a chain of links whose targets are long paths.
const MAX_WALK_ENTRIES: usize = 1024;

/// The `GetDriveTypeW` result for a local fixed drive, the only kind a followed link may use.
const DRIVE_FIXED: u32 = 3;

#[cfg(target_os = "windows")]
pub(super) fn classify_local_target(path: &Path) -> PathOpenDecision {
    classify_windows_target(path)
}

#[cfg(any(target_os = "windows", test))]
fn windows_file_name(path: &Path) -> Option<&str> {
    path.to_str()?.rsplit(['/', '\\']).next().filter(|name| !name.is_empty())
}

/// Whether `name` is one plain Windows filename: not empty, with no alternate-stream colon, no
/// trailing dot or space that Win32 would strip, and no control or reserved character.
fn windows_name_allowed(name: &str) -> bool {
    let reserved = name.is_empty()
        || name.contains(':')
        || name.ends_with(['.', ' '])
        || name.chars().any(|character| {
            character.is_control() || matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
        });
    !reserved
}

#[cfg(any(target_os = "windows", test))]
pub(super) fn windows_path_policy(path: &Path) -> PathOpenDecision {
    let Some(name) = windows_file_name(path) else {
        // When: `windows_file_name` cannot produce one nonempty component, reject an unclassifiable ShellExecute target.
        return PathOpenDecision::Blocked;
    };
    if !windows_name_allowed(name) {
        // When: `windows_name_allowed` rejects `name` for ADS or reserved syntax, block Windows normalization and alternate-stream ambiguity.
        return PathOpenDecision::Blocked;
    }
    PathOpenDecision::Openable(PathKind::File)
}

/// What the link walk finds at one path, without following a link there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalEntry {
    /// An ordinary folder.
    Directory,
    /// An ordinary file.
    File,
    /// A name-surrogate reparse point, such as a symlink or junction; `directory` is the link's
    /// own folder flag.
    Link { directory: bool },
    /// Nothing exists at the path.
    Missing,
    /// Another reparse point, such as a cloud-file placeholder, or an entry that cannot be read.
    Refused,
}

/// The filesystem and volume reads the link walk makes, injectable so tests can simulate links,
/// missing entries and network drives without real volumes.
trait LocalLinkProbe {
    /// Read the entry at `path` without following a link there.
    fn entry(&mut self, path: &str) -> LocalEntry;
    /// Read the raw target text of the symlink or junction at `path`, without following it.
    fn link_target(&mut self, path: &str) -> Option<String>;
    /// The `GetDriveTypeW` result for the root of `drive`.
    fn drive_type(&mut self, drive: char) -> u32;
}

/// A link target that names a local path: drive-absolute, or relative to the link's folder,
/// climbing `parent_steps` folders before descending through `names`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LinkTarget {
    Absolute { drive: char, names: Vec<String> },
    Relative { parent_steps: usize, names: Vec<String> },
}

/// Whether a `GetDriveTypeW` result is a local fixed drive. A followed link, and the target it
/// names, must both be on one: a network share, removable or optical media, a RAM disk, or an
/// unknown root is refused.
fn drive_type_is_local_fixed(drive_type: u32) -> bool {
    drive_type == DRIVE_FIXED
}

/// Parse the raw target text `read_link` reports for a symlink or junction. A drive-absolute
/// target, bare or behind a `\??\` or `\\?\` prefix, and a target relative to the link's folder
/// name a local path; UNC, device, volume, drive-relative and root-relative targets, `/`
/// separators, `..` after a name, and alternate-stream or reserved names return `None`.
fn parse_link_target(raw: &str) -> Option<LinkTarget> {
    if raw.contains('/') {
        // When: `raw` contains `/`, refuse it; NT link text separates names only with `\`.
        return None;
    }
    let unprefixed = raw.strip_prefix(r"\??\").or_else(|| raw.strip_prefix(r"\\?\")).unwrap_or(raw);
    if let Some((drive, rest)) = split_drive_root(unprefixed) {
        // When: `split_drive_root` finds a drive root in `unprefixed`, the target is drive-absolute on `drive`.
        return parse_absolute_names(rest).map(|names| LinkTarget::Absolute { drive, names });
    }
    if unprefixed.len() != raw.len() || unprefixed.starts_with('\\') || unprefixed.contains(':') {
        // When: `unprefixed` lost a prefix without a drive root, starts with `\`, or holds `:`, it is UNC, device, volume, rooted or drive-relative.
        return None;
    }
    let (parent_steps, names) = parse_relative_names(unprefixed)?;
    Some(LinkTarget::Relative { parent_steps, names })
}

/// Split `X:\rest` or `X:/rest` into the upper-case drive letter and the text after its root.
fn split_drive_root(path: &str) -> Option<(char, &str)> {
    let bytes = path.as_bytes();
    let rooted = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    if !rooted {
        // When: `rooted` is false, `path` has no drive letter followed by `:` and a separator.
        return None;
    }
    Some((char::from(bytes[0]).to_ascii_uppercase(), &path[3..]))
}

/// Split the names below an absolute target's drive root. NT resolves such a target without Win32
/// normalization, so a `.`, `..`, empty, or reserved name refuses the whole target.
fn parse_absolute_names(rest: &str) -> Option<Vec<String>> {
    let rest = rest.strip_suffix('\\').unwrap_or(rest);
    if rest.is_empty() {
        // When: `rest` is empty after one trailing separator, the target is the drive root itself.
        return Some(Vec::new());
    }
    rest.split('\\').map(|name| windows_name_allowed(name).then(|| name.to_string())).collect()
}

/// Split a target relative to the link's folder into leading `..` climbs and the names below. A
/// `..` after a name is refused: if that name is itself a link, the folder `..` reaches depends
/// on whether Windows resolves the link first, so the walk cannot know it.
fn parse_relative_names(target: &str) -> Option<(usize, Vec<String>)> {
    let target = target.strip_suffix('\\').unwrap_or(target);
    let mut parent_steps = 0;
    let mut names = Vec::new();
    for part in target.split('\\') {
        match part {
            "." => {
                // When: `part` is `.`, it names the folder already reached and adds no name.
            }
            ".." if names.is_empty() => parent_steps += 1,
            _ if windows_name_allowed(part) => names.push(part.to_string()),
            _ => {
                // When: `part` is a `..` after a name, empty, or reserved, refuse the target rather than guess its folder.
                return None;
            }
        }
    }
    Some((parent_steps, names))
}

/// Split a drive-absolute path into its upper-case drive letter and names, resolving `.` and `..`
/// lexically as Win32 does before any lookup; UNC, device, rooted and relative paths return `None`.
fn parse_local_path(path: &str) -> Option<(char, Vec<&str>)> {
    let (drive, rest) = split_drive_root(path)?;
    let mut names = Vec::new();
    for part in rest.split(['\\', '/']) {
        match part {
            "" | "." => {
                // When: `part` is empty or `.`, it names the folder already reached.
            }
            ".." => {
                // Clamp at the drive root, as Win32 path normalization does.
                names.pop();
            }
            _ => names.push(part),
        }
    }
    Some((drive, names))
}

/// One pending step of a link walk.
enum WalkStep {
    /// Look up `name` below the resolved folder; `last` marks the original path's final name.
    Name { name: String, last: bool },
    /// The end of a followed link's target, which must match the link's own folder flag.
    LinkEnd { directory: bool },
}

/// The state of one link walk. `resolved` holds only folders proven to be ordinary local
/// folders, so no entry read below them passes through a link.
struct LinkWalk<'probe> {
    probe: &'probe mut dyn LocalLinkProbe,
    drive: char,
    resolved: Vec<String>,
    kind: PathKind,
    pending: Vec<WalkStep>,
    link_hops: usize,
    entries_read: usize,
    dangling_blocks: bool,
}

/// Classify a drive-absolute path by walking it from its drive root, reading each entry only
/// after every folder above it is proven to be an ordinary local folder. A symlink or junction is
/// replaced by its target only when the drive holding it and the drive its target names are both
/// local fixed drives. `Missing` means nothing exists at the path; `Blocked` covers a dangling or
/// looping link, a refused target, another reparse point, and a path that is not drive-absolute.
fn resolve_local_links(
    path: &str,
    probe: &mut dyn LocalLinkProbe,
) -> Result<PathKind, PathOpenDecision> {
    let Some((drive, names)) = parse_local_path(path) else {
        // When: `parse_local_path` finds no `X:\` root, refuse a UNC, device, rooted or relative path before any read.
        return Err(PathOpenDecision::Blocked);
    };
    let last_index = names.len().saturating_sub(1);
    let mut walk = LinkWalk {
        probe,
        drive,
        resolved: Vec::new(),
        kind: PathKind::Directory,
        pending: Vec::new(),
        link_hops: 0,
        entries_read: 0,
        dangling_blocks: false,
    };
    walk.pending.extend(
        names.into_iter().enumerate().rev().map(|(index, name)| WalkStep::Name {
            name: name.to_string(),
            last: index == last_index,
        }),
    );
    walk.enter_root()?;
    while let Some(step) = walk.pending.pop() {
        walk.take(step)?;
    }
    Ok(walk.kind)
}

impl LinkWalk<'_> {
    /// The local path of `name` below the resolved folders, or of the last resolved folder.
    fn render(&self, name: Option<&str>) -> String {
        let parts = self.resolved.iter().map(String::as_str).chain(name).collect::<Vec<_>>();
        format!("{}:\\{}", self.drive, parts.join("\\"))
    }

    /// Read one entry, refusing the walk once it has read `MAX_WALK_ENTRIES` entries.
    fn read_entry(&mut self, path: &str) -> Result<LocalEntry, PathOpenDecision> {
        self.entries_read += 1;
        if self.entries_read > MAX_WALK_ENTRIES {
            // When: `entries_read` passes `MAX_WALK_ENTRIES`, refuse a chain of long link targets instead of reading on.
            return Err(PathOpenDecision::Blocked);
        }
        Ok(self.probe.entry(path))
    }

    /// The decision where nothing exists: a dangling final link blocks, as on macOS and Linux,
    /// while any other absent path is missing, so a shorter candidate can still be tried.
    fn absent(&self) -> PathOpenDecision {
        if self.dangling_blocks {
            PathOpenDecision::Blocked
        } else {
            // When: `dangling_blocks` is false, no final link is being followed, so the path is just missing.
            PathOpenDecision::Missing
        }
    }

    /// Restart the walk at the root of the current drive, which must be an ordinary folder.
    fn enter_root(&mut self) -> Result<(), PathOpenDecision> {
        self.resolved.clear();
        self.kind = PathKind::Directory;
        let root = self.render(None);
        match self.read_entry(&root)? {
            LocalEntry::Directory => Ok(()),
            LocalEntry::Missing => Err(self.absent()),
            LocalEntry::File | LocalEntry::Link { .. } | LocalEntry::Refused => {
                Err(PathOpenDecision::Blocked)
            }
        }
    }

    /// Apply one pending step, queueing a followed link's target ahead of the rest.
    fn take(&mut self, step: WalkStep) -> Result<(), PathOpenDecision> {
        match step {
            WalkStep::Name { name, last } => self.take_name(name, last),
            WalkStep::LinkEnd { directory } => self.end_link(directory),
        }
    }

    /// Check that a followed link reached the kind its own folder flag promises: a junction or
    /// folder symlink a folder, a file symlink a file. Explorer treats a link by its own flag, so
    /// a mismatch is refused rather than guessed.
    fn end_link(&self, directory: bool) -> Result<(), PathOpenDecision> {
        if directory == (self.kind == PathKind::Directory) {
            Ok(())
        } else {
            // When: the link's `directory` flag disagrees with the resolved `kind`, refuse the link.
            Err(PathOpenDecision::Blocked)
        }
    }

    /// Look up `name` below the resolved folder, following it when it is a link.
    fn take_name(&mut self, name: String, last: bool) -> Result<(), PathOpenDecision> {
        if self.kind == PathKind::File {
            // When: `kind` is `File`, nothing exists below it; Windows reports such a path as not found.
            return Err(self.absent());
        }
        let path = self.render(Some(name.as_str()));
        match self.read_entry(&path)? {
            LocalEntry::Directory => {
                self.resolved.push(name);
                self.kind = PathKind::Directory;
                Ok(())
            }
            LocalEntry::File => {
                self.resolved.push(name);
                self.kind = PathKind::File;
                Ok(())
            }
            LocalEntry::Missing => Err(self.absent()),
            LocalEntry::Refused => Err(PathOpenDecision::Blocked),
            LocalEntry::Link { directory } => {
                self.dangling_blocks |= last;
                self.follow_link(&path, directory)
            }
        }
    }

    /// Follow the symlink or junction at `path`. It is refused unless the walk stays within
    /// `MAX_LINK_HOPS`, the link's drive is a local fixed drive, and its target parses as a local
    /// form; otherwise the target's names are queued, then a check of the link's folder flag.
    fn follow_link(&mut self, path: &str, directory: bool) -> Result<(), PathOpenDecision> {
        self.link_hops += 1;
        if self.link_hops > MAX_LINK_HOPS {
            // When: `link_hops` passes `MAX_LINK_HOPS`, refuse a loop or a chain longer than Windows resolves.
            return Err(PathOpenDecision::Blocked);
        }
        let drive_type = self.probe.drive_type(self.drive);
        if !drive_type_is_local_fixed(drive_type) {
            // When: `drive_type_is_local_fixed` rejects the link's own drive, refuse it; a remote or removable volume interprets its target.
            return Err(PathOpenDecision::Blocked);
        }
        let raw_target = self.probe.link_target(path);
        let Some(target) = raw_target.as_deref().and_then(parse_link_target) else {
            // When: the target is unreadable or `parse_link_target` refuses its UNC, device, volume or rooted text.
            return Err(PathOpenDecision::Blocked);
        };
        self.pending.push(WalkStep::LinkEnd { directory });
        let names = match target {
            LinkTarget::Absolute { drive, names } => {
                self.enter_drive(drive)?;
                names
            }
            LinkTarget::Relative { parent_steps, names } => {
                self.climb(parent_steps)?;
                names
            }
        };
        self.pending
            .extend(names.into_iter().rev().map(|name| WalkStep::Name { name, last: false }));
        Ok(())
    }

    /// Move the walk to the root of `drive`, which must be a local fixed drive.
    fn enter_drive(&mut self, drive: char) -> Result<(), PathOpenDecision> {
        if !drive_type_is_local_fixed(self.probe.drive_type(drive)) {
            // When: `drive_type_is_local_fixed` rejects the target `drive`, refuse it before reading anything on a mapped share.
            return Err(PathOpenDecision::Blocked);
        }
        self.drive = drive;
        self.enter_root()
    }

    /// Climb `parent_steps` folders from the link's folder, never above the drive root.
    fn climb(&mut self, parent_steps: usize) -> Result<(), PathOpenDecision> {
        let Some(kept) = self.resolved.len().checked_sub(parent_steps) else {
            // When: `checked_sub` shows `parent_steps` would climb above the drive root, refuse rather than clamp.
            return Err(PathOpenDecision::Blocked);
        };
        self.resolved.truncate(kept);
        self.kind = PathKind::Directory;
        Ok(())
    }
}

/// The production probe: `symlink_metadata` and `read_link` open the entry itself with
/// `FILE_FLAG_OPEN_REPARSE_POINT`, so neither follows a link at `path`.
#[cfg(target_os = "windows")]
struct NativeLinkProbe;

#[cfg(target_os = "windows")]
impl LocalLinkProbe for NativeLinkProbe {
    fn entry(&mut self, path: &str) -> LocalEntry {
        use std::os::windows::fs::{FileTypeExt, MetadataExt};
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // When: `error.kind()` is `NotFound`, nothing exists at `path`.
                return LocalEntry::Missing;
            }
            Err(_) => {
                // When: `symlink_metadata` fails for another reason, refuse an unreadable entry instead of inferring its identity.
                return LocalEntry::Refused;
            }
        };
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            // std reports any name-surrogate reparse point as a symlink; `link_target` then reads
            // only a symlink's or a junction's target, so another link-like tag is refused.
            LocalEntry::Link { directory: file_type.is_symlink_dir() }
        } else if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            // When: `file_attributes` carries a reparse point that is no link, refuse a placeholder or other redirected entry.
            LocalEntry::Refused
        } else if metadata.is_dir() {
            // When: `is_dir` holds for an entry that is no reparse point, it is an ordinary folder.
            LocalEntry::Directory
        } else {
            // When: the entry is neither a link, another reparse point, nor `is_dir`, it is an ordinary file.
            LocalEntry::File
        }
    }

    fn link_target(&mut self, path: &str) -> Option<String> {
        // `read_link` decodes only symlink and junction reparse data; any other tag is an error.
        std::fs::read_link(path).ok()?.into_os_string().into_string().ok()
    }

    fn drive_type(&mut self, drive: char) -> u32 {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::GetDriveTypeW;
        let root = format!("{drive}:\\").encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        // SAFETY: `root` is a NUL-terminated UTF-16 drive root that stays live through the call.
        unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) }
    }
}

#[cfg(target_os = "windows")]
fn classify_windows_target(path: &Path) -> PathOpenDecision {
    classify_windows_target_with(path, &mut NativeLinkProbe)
}

/// Classify `path` with `probe`: walk its links, then apply the filename policy to the path's own
/// final name, which is the name Explorer selects or opens.
#[cfg(target_os = "windows")]
fn classify_windows_target_with(path: &Path, probe: &mut dyn LocalLinkProbe) -> PathOpenDecision {
    let Some(text) = path.to_str() else {
        // When: `to_str` fails, a non-UTF-8 path can be neither walked nor checked by filename policy.
        return PathOpenDecision::Blocked;
    };
    let kind = match resolve_local_links(text, probe) {
        Ok(kind) => kind,
        Err(decision) => {
            // When: `resolve_local_links` returns `Err`, retain its missing-or-blocked decision unchanged.
            return decision;
        }
    };
    if kind == PathKind::Directory && path.parent().is_none() {
        // When: kind is Directory at the drive root, no final filename exists for extension policy.
        return PathOpenDecision::Openable(PathKind::Directory);
    }
    if windows_path_policy(path).is_blocked() {
        PathOpenDecision::Blocked
    } else {
        // When: `windows_path_policy` does not block `path`, restore the walk-derived file-or-directory `kind`.
        PathOpenDecision::Openable(kind)
    }
}

/// Revalidate a directory navigation at dispatch time: the path must still walk, link by link, to
/// the local directory the probe authorized.
#[cfg(target_os = "windows")]
fn validate_windows_directory(path: &Path, expected_decision: PathOpenDecision) -> io::Result<()> {
    if expected_decision != PathOpenDecision::Openable(PathKind::Directory) {
        // When: `expected_decision` is not a directory, the navigation dispatcher must never launch a file.
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsupported Windows action"));
    }
    if classify_windows_target(path) != expected_decision {
        // When: `classify_windows_target` no longer returns `expected_decision`, reject a changed or newly blocked target.
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "changed or blocked Windows target",
        ));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(super) fn reveal_native_file(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName};
    let target = path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
    // SAFETY: this worker owns its COM apartment; target and PIDL remain live until selection returns, then are released once.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().map_err(io::Error::other)?;
        let mut pidl = std::ptr::null_mut();
        let result = SHParseDisplayName(PCWSTR(target.as_ptr()), None, &mut pidl, 0, None)
            .and_then(|()| SHOpenFolderAndSelectItems(pidl, None, 0));
        CoTaskMemFree(Some(pidl.cast()));
        CoUninitialize();
        result.map_err(io::Error::other)
    }
}

#[cfg(target_os = "windows")]
pub(super) fn open_native_path(path: &Path, expected_decision: PathOpenDecision) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    validate_windows_directory(path, expected_decision)?;
    let verb = "open\0".encode_utf16().collect::<Vec<_>>();
    let target = path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
    // SAFETY: this dedicated worker owns its COM apartment; all UTF-16 buffers
    // remain live through the synchronous SEE_MASK_NOASYNC call.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().map_err(io::Error::other)?;
        let mut info = SHELLEXECUTEINFOW {
            cbSize: u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>()).unwrap_or(u32::MAX),
            fMask: SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(target.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let result = ShellExecuteExW(&mut info).map_err(io::Error::other);
        CoUninitialize();
        result
    }
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod windows_tests;
