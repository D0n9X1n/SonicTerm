//! Windows path probes and direct-open. Classification walks a drive-absolute path one part at a
//! time and keeps every part it opens held until the classification or the native action ends.
//!
//! The drive letter is resolved once, with `QueryDosDeviceW`, a namespace query that touches
//! neither the filesystem nor the network, and its first target must be exactly
//! `\Device\HarddiskVolume<digits>`. A mapped network drive, a `subst` drive, an optical or RAM
//! drive, and a failed or empty query are refused before any other call. The root is opened through
//! that NT device path, never through the letter again, and `FileFsDeviceInformation` on the root
//! handle must report `FILE_DEVICE_DISK` without `FILE_REMOTE_DEVICE`. `.` and `..` in the input
//! resolve by text first, as Win32 path normalization does. Every later part is opened with
//! `NtCreateFile` by one validated name relative to its parent's held handle, with
//! `OBJ_DONT_REPARSE` and `FILE_OPEN_REPARSE_POINT`, so no open passes through a link and nothing
//! after the root is opened by path. Microsoft's `OBJECT_ATTRIBUTES` documentation names no first
//! Windows build for `OBJ_DONT_REPARSE`. Every attempt keeps the flag, so a build that rejects it
//! cannot open the root or a child through a flag-free retry. `nt_open` reports `Missing` for not-found
//! statuses and `Refused` for other failures after the access-denied retry; the returned status
//! determines the classification.
//!
//! A symlink or junction is followed only between local fixed disks: the link's own volume must not
//! be removable, and its target must be `\??\X:\…` on a drive that passes the same root checks and
//! is not removable, or a symlink target relative to the link's folder, resolved by text against
//! the held link-free path. Volume-GUID mount folders, UNC, device and other reparse targets are
//! refused before anything they name is opened, and other reparse points are refused outright. The
//! walk therefore never follows a link off a local fixed disk and never opens a remote volume.
//!
//! Each open asks for `FILE_READ_DATA` (`FILE_LIST_DIRECTORY` on a folder) with
//! `FILE_READ_ATTRIBUTES | SYNCHRONIZE`, and only when that is denied retries with `FILE_EXECUTE`
//! (`FILE_TRAVERSE` on a folder); it never asks for write or delete access, and shares read and
//! write but not delete. An attribute-only open takes no part in share-access checks; either
//! read-class right makes this one count, so nobody can rename or delete a held part, since both
//! need `DELETE` access. A part the user can neither read nor execute, or one another program
//! holds without read sharing, is refused. `FSCTL_SET_REPARSE_POINT` fails with
//! `STATUS_DIRECTORY_NOT_EMPTY` on a folder that has entries (MS-FSA), and every held folder
//! contains the next held part, so no held folder above the final part can be renamed, removed or
//! turned into a link in place. The final part can change in place, but the walk opens it with
//! `FILE_OPEN_REPARSE_POINT` and never opens anything through it.
//!
//! Dispatch runs the same walk and hands `SHParseDisplayName` or `ShellExecuteExW` the link-free
//! path built from the verified drive letter and the names the walk opened, never the input text,
//! while every part stays held, so a file reached through a link is selected in its real folder.
//! The shell, Explorer and the file's handler then open that path themselves: the final part can
//! still change in place, and nothing is held once the shell call returns. A process in the user's
//! own logon session can redefine drive letters and reach the network directly, so it is out of
//! scope. The module is declared as `windows_os` so the `windows::` paths here keep naming the
//! `windows` crate.

use super::*;

/// Most symlinks and junctions one classification follows. Windows resolves at most 31 reparse
/// points on a path whose links name fully qualified targets, so a longer chain, or a loop, is refused.
const MAX_LINK_HOPS: usize = 31;

/// Most entries one classification opens, bounding a chain of links whose targets are long paths.
const MAX_WALK_ENTRIES: usize = 1024;

/// The `FILE_ATTRIBUTE_DIRECTORY` bit: the held entry is a folder.
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// The `FILE_ATTRIBUTE_REPARSE_POINT` bit: the held entry carries a reparse point.
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// The reparse tag of a symlink.
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

/// The reparse tag of a junction or a volume mount folder.
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// The `SYMLINK_FLAG_RELATIVE` bit of a symlink's reparse data: its target is relative to its folder.
const SYMLINK_FLAG_RELATIVE: u32 = 0x1;

/// The `FILE_DEVICE_DISK` device type `FileFsDeviceInformation` reports for a disk volume.
const FILE_DEVICE_DISK: u32 = 0x7;

/// The `FILE_REMOVABLE_MEDIA` device characteristic.
const FILE_REMOVABLE_MEDIA: u32 = 0x1;

/// The `FILE_REMOTE_DEVICE` device characteristic of a network volume.
const FILE_REMOTE_DEVICE: u32 = 0x10;

/// Classify a local path for the probe worker, walking it under custody and releasing every handle.
#[cfg(target_os = "windows")]
pub(super) fn classify_local_target(path: &Path) -> PathOpenDecision {
    classify_windows_target_with(path, &mut NativeLinkProbe)
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

/// Whether Win32 would read `name` as a DOS device rather than an entry: `CON`, `PRN`, `AUX`,
/// `NUL`, `CONIN$`, `CONOUT$`, or `COM` or `LPT` with one digit, in any case and with any
/// extension. The walk never hands such a name to the shell, which could reach a redirected port.
fn is_dos_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ').to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        // When: `stem` `matches!` a fixed device name such as `NUL`, which Win32 maps to that device.
        return true;
    }
    let mut characters = stem.chars();
    let prefix = characters.by_ref().take(3).collect::<String>();
    let digit = characters.next();
    matches!(prefix.as_str(), "COM" | "LPT")
        && matches!(digit, Some('1'..='9' | '¹' | '²' | '³'))
        && characters.next().is_none()
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

/// What the walk finds at one held entry, read from the entry's own handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalEntry {
    /// An ordinary folder.
    Directory,
    /// An ordinary file.
    File,
    /// A symlink or junction; `directory` is the link's own folder flag.
    Link { directory: bool },
    /// Another reparse point, such as a cloud-file placeholder.
    Refused,
}

/// The attributes and reparse tag `FileAttributeTagInfo` reports for one held entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EntryAttributes {
    attributes: u32,
    reparse_tag: u32,
}

/// Classify a held entry from its own attributes and reparse tag. Only a symlink or junction tag
/// is a link; any other reparse point is refused, since the walk cannot check where it leads.
fn entry_kind(entry: EntryAttributes) -> LocalEntry {
    let directory = entry.attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    let reparse = entry.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    match (reparse, entry.reparse_tag) {
        (false, _) if directory => LocalEntry::Directory,
        (false, _) => LocalEntry::File,
        (true, IO_REPARSE_TAG_SYMLINK | IO_REPARSE_TAG_MOUNT_POINT) => {
            LocalEntry::Link { directory }
        }
        (true, _) => LocalEntry::Refused,
    }
}

/// The device type and characteristics `FileFsDeviceInformation` reports for a volume root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VolumeDevice {
    device_type: u32,
    characteristics: u32,
}

/// Whether a volume root may start a walk: a disk that is not remote. With `fixed_only`, as for a
/// link's target, it must not be removable either, so links are followed only between fixed disks.
fn volume_is_local_disk(volume: VolumeDevice, fixed_only: bool) -> bool {
    let refused_characteristics = if fixed_only {
        FILE_REMOTE_DEVICE | FILE_REMOVABLE_MEDIA
    } else {
        // When: `fixed_only` is false, the walk starts on this volume rather than following a link onto it, so only a remote device is refused.
        FILE_REMOTE_DEVICE
    };
    volume.device_type == FILE_DEVICE_DISK && volume.characteristics & refused_characteristics == 0
}

/// Whether a `QueryDosDeviceW` target is exactly `\Device\HarddiskVolume<digits>`, the only drive
/// target the walk opens. Network redirectors such as `\Device\Mup` also live under `\Device`, so
/// that prefix alone is not enough; `subst`, optical, RAM-disk and empty targets are refused too.
fn is_hard_disk_volume(target: &str) -> bool {
    target.strip_prefix(r"\Device\HarddiskVolume").is_some_and(|number| {
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// The result of opening one entry.
enum OpenOutcome<Handle> {
    /// The entry exists and is now held.
    Held(Handle),
    /// Nothing exists at the name.
    Missing,
    /// The entry exists but cannot be held, for example because access is denied.
    Refused,
}

/// The namespace, volume and handle operations the walk makes, injectable so tests can simulate
/// volumes, links and missing entries without real drives. Only `dos_device` names a drive letter
/// and only `open_root` names an NT device path; every other open names one entry below a held
/// parent, so the walk cannot pass a full path after the root.
trait LocalLinkProbe {
    /// A held entry; dropping it closes the entry.
    type Handle;
    /// The first target `QueryDosDeviceW` reports for drive `letter`, or `None` when the query fails.
    fn dos_device(&mut self, letter: char) -> Option<String>;
    /// Open the root folder of the NT volume `device`, never through a drive letter.
    fn open_root(&mut self, device: &str) -> OpenOutcome<Self::Handle>;
    /// Open the single entry `name` in the held folder `parent`, without following a link there.
    fn open_child(&mut self, parent: &Self::Handle, name: &str) -> OpenOutcome<Self::Handle>;
    /// The device type and characteristics of the volume holding `root`, or `None` when unreadable.
    fn volume_device(&mut self, root: &Self::Handle) -> Option<VolumeDevice>;
    /// The attributes and reparse tag of the held `entry`, or `None` when unreadable.
    fn attributes(&mut self, entry: &Self::Handle) -> Option<EntryAttributes>;
    /// The target text of the held symlink or junction `link`, as `decode_reparse_target` reads it.
    fn link_target(&mut self, link: &Self::Handle) -> Option<String>;
}

/// A link target that names a local path: drive-absolute, or relative to the link's folder,
/// climbing `parent_steps` folders before descending through `names`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LinkTarget {
    Absolute { drive: char, names: Vec<String> },
    Relative { parent_steps: usize, names: Vec<String> },
}

/// Parse the target text of a symlink or junction. Only an NT drive path `\??\X:\…` and a target
/// relative to the link's folder name a local path; bare and `\\?\` drive paths, UNC, device,
/// volume-GUID, drive-relative and root-relative targets, `/` separators, `..` after a name, and
/// alternate-stream or reserved names return `None`.
fn parse_link_target(raw: &str) -> Option<LinkTarget> {
    if raw.contains('/') {
        // When: `raw` contains `/`, refuse it; NT link text separates names only with `\`.
        return None;
    }
    if let Some(nt_path) = raw.strip_prefix(r"\??\") {
        // When: `raw` is an NT path, only a drive root below `\??\` is local; UNC, volume-GUID and device forms fail `split_drive_root`.
        let (drive, rest) = split_drive_root(nt_path)?;
        return parse_absolute_names(rest).map(|names| LinkTarget::Absolute { drive, names });
    }
    if raw.starts_with('\\') || raw.contains(':') {
        // When: `raw` starts with `\` or holds `:`, it is a `\\?\`, UNC, device, rooted, drive or drive-relative form.
        return None;
    }
    let (parent_steps, names) = parse_relative_names(raw)?;
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

/// The little-endian `u16` at `offset` in `bytes`, widened, or `None` past the end.
fn read_u16_le(bytes: &[u8], offset: usize) -> Option<usize> {
    let pair: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(usize::from(u16::from_le_bytes(pair)))
}

/// Decode the substitute name of a symlink or junction from the `REPARSE_DATA_BUFFER` that
/// `FSCTL_GET_REPARSE_POINT` returns, following its documented layout. A symlink whose relative
/// flag disagrees with whether its text starts with `\`, a junction whose text does not, any other
/// tag, and a buffer too short for the lengths it declares return `None`.
fn decode_reparse_target(buffer: &[u8]) -> Option<String> {
    let tag_bytes: [u8; 4] = buffer.get(..4)?.try_into().ok()?;
    let data_length = read_u16_le(buffer, 4)?;
    let data = buffer.get(8..8 + data_length)?;
    let (path_start, relative) = match u32::from_le_bytes(tag_bytes) {
        IO_REPARSE_TAG_SYMLINK => {
            let flag_bytes: [u8; 4] = data.get(8..12)?.try_into().ok()?;
            (12, u32::from_le_bytes(flag_bytes) & SYMLINK_FLAG_RELATIVE != 0)
        }
        IO_REPARSE_TAG_MOUNT_POINT => (8, false),
        _ => {
            // When: `tag_bytes` hold neither a symlink nor a junction tag, so the data names no target the walk can check.
            return None;
        }
    };
    let name_start = path_start + read_u16_le(data, 0)?;
    let name_bytes = data.get(name_start..name_start + read_u16_le(data, 2)?)?;
    let (pairs, remainder) = name_bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        // When: `remainder` holds a byte, the substitute name has an odd byte length and cannot be UTF-16 text.
        return None;
    }
    let units = pairs.iter().copied().map(u16::from_le_bytes).collect::<Vec<_>>();
    let target = String::from_utf16(&units).ok()?;
    if target.is_empty() || relative == target.starts_with('\\') {
        // When: `target` is empty, or `relative` disagrees with a leading `\`, the link names no path the walk can place.
        return None;
    }
    Some(target)
}

/// One pending step of a custody walk.
enum WalkStep {
    /// Open `name` in the held folder; `last` marks the original path's final name.
    Name { name: String, last: bool },
    /// The end of a followed link's target, which must match the link's own folder flag.
    LinkEnd { directory: bool },
}

/// A walked path held open: the kind the walk reached, the link-free path to hand the shell, and
/// every entry the walk opened, on every chain it crossed. Dropping it releases them all.
struct HeldTarget<Handle> {
    kind: PathKind,
    dispatch_path: String,
    _custody: Vec<Handle>,
}

/// The state of one custody walk. `chain` holds the drive root, each folder of the link-free path
/// and then the final entry, one more handle than `names`, the link-free names below the root.
/// `custody` holds every other entry the walk opened: followed links, and the parts of a chain that
/// a followed link or a `..` left behind.
struct CustodyWalk<'probe, Probe: LocalLinkProbe> {
    probe: &'probe mut Probe,
    drive: char,
    names: Vec<String>,
    chain: Vec<Probe::Handle>,
    custody: Vec<Probe::Handle>,
    removable: bool,
    kind: PathKind,
    pending: Vec<WalkStep>,
    link_hops: usize,
    entries_opened: usize,
    dangling_blocks: bool,
}

/// Walk a drive-absolute path from its drive root, opening each entry below its held parent and
/// holding every entry it opens. A symlink or junction is replaced by its target only between
/// local fixed disks. `Missing` means nothing exists at the path; `Blocked` covers a drive that is
/// not a local disk volume, a dangling or looping link, a refused target, another reparse point, a
/// name the shell would read as another entry, and a path that is not drive-absolute.
fn hold_local_target<Probe: LocalLinkProbe>(
    path: &str,
    probe: &mut Probe,
) -> Result<HeldTarget<Probe::Handle>, PathOpenDecision> {
    let Some((drive, names)) = parse_local_path(path) else {
        // When: `parse_local_path` finds no `X:\` root, refuse a UNC, device, rooted or relative path before any call.
        return Err(PathOpenDecision::Blocked);
    };
    let last_index = names.len().saturating_sub(1);
    let pending = names
        .into_iter()
        .enumerate()
        .rev()
        .map(|(index, name)| WalkStep::Name { name: name.to_string(), last: index == last_index })
        .collect();
    let mut walk = CustodyWalk {
        probe,
        drive,
        names: Vec::new(),
        chain: Vec::new(),
        custody: Vec::new(),
        removable: false,
        kind: PathKind::Directory,
        pending,
        link_hops: 0,
        entries_opened: 0,
        dangling_blocks: false,
    };
    walk.enter_drive(drive, false)?;
    while let Some(step) = walk.pending.pop() {
        walk.take(step)?;
    }
    Ok(walk.finish())
}

impl<Probe: LocalLinkProbe> CustodyWalk<'_, Probe> {
    /// Count one open, refusing the walk once it has opened `MAX_WALK_ENTRIES` entries.
    fn count_open(&mut self) -> Result<(), PathOpenDecision> {
        self.entries_opened += 1;
        if self.entries_opened > MAX_WALK_ENTRIES {
            // When: `entries_opened` passes `MAX_WALK_ENTRIES`, refuse a chain of long link targets instead of opening on.
            return Err(PathOpenDecision::Blocked);
        }
        Ok(())
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

    /// Restart the walk at the root of `letter`. The letter is resolved once, to an exact
    /// `\Device\HarddiskVolume<digits>` target, and the root is opened through that NT path, so a
    /// later change to the letter cannot redirect the walk. The volume must be a disk that is not
    /// remote and, with `fixed_only`, as for a link's target, not removable.
    fn enter_drive(&mut self, letter: char, fixed_only: bool) -> Result<(), PathOpenDecision> {
        let device = self.probe.dos_device(letter);
        let Some(device) = device.filter(|device| is_hard_disk_volume(device)) else {
            // When: `is_hard_disk_volume` rejects the `device` of `letter`, refuse a network, `subst`, optical or RAM drive before any other call.
            return Err(PathOpenDecision::Blocked);
        };
        self.count_open()?;
        let root = match self.probe.open_root(&device) {
            OpenOutcome::Held(root) => root,
            OpenOutcome::Missing => {
                // When: `open_root` finds no volume at `device`, the drive vanished after `dos_device` resolved it.
                return Err(self.absent());
            }
            OpenOutcome::Refused => {
                // When: `open_root` cannot hold the root of `device`, refuse the drive rather than infer its identity.
                return Err(PathOpenDecision::Blocked);
            }
        };
        let volume = self.probe.volume_device(&root);
        let Some(volume) = volume.filter(|volume| volume_is_local_disk(*volume, fixed_only)) else {
            // When: `volume_device` reports a remote or non-disk volume, or a removable one where `fixed_only` holds, refuse before opening below `root`.
            return Err(PathOpenDecision::Blocked);
        };
        if self.probe.attributes(&root).map(entry_kind) != Some(LocalEntry::Directory) {
            // When: the `attributes` of `root` are unreadable or not an ordinary folder, no name can be opened below it.
            return Err(PathOpenDecision::Blocked);
        }
        self.custody.append(&mut self.chain);
        self.chain.push(root);
        self.names.clear();
        self.drive = letter;
        self.removable = volume.characteristics & FILE_REMOVABLE_MEDIA != 0;
        self.kind = PathKind::Directory;
        Ok(())
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

    /// Open `name` in the held folder and hold it, following it when it is a link. An existing
    /// entry whose name the shell would read as another entry, or as a device, is refused.
    fn take_name(&mut self, name: String, last: bool) -> Result<(), PathOpenDecision> {
        if self.kind == PathKind::File {
            // When: `kind` is `File`, nothing exists below it; Windows reports such a path as not found.
            return Err(self.absent());
        }
        if name.contains([':', '\0']) {
            // When: `name` holds `:` or NUL, it names a stream or nothing, never a file or folder, so it is not opened.
            return Err(self.absent());
        }
        self.count_open()?;
        let Some(parent) = self.chain.last() else {
            // When: `chain` is empty, no folder is held to open `name` in; `enter_drive` always holds a root, so refuse.
            return Err(PathOpenDecision::Blocked);
        };
        let entry = match self.probe.open_child(parent, &name) {
            OpenOutcome::Held(entry) => entry,
            OpenOutcome::Missing => {
                // When: `open_child` finds nothing named `name` in the held folder, the path is absent.
                return Err(self.absent());
            }
            OpenOutcome::Refused => {
                // When: `open_child` cannot hold `name`, refuse an unreadable entry instead of inferring its identity.
                return Err(PathOpenDecision::Blocked);
            }
        };
        if !windows_name_allowed(&name) || is_dos_device_name(&name) {
            // When: `name` exists but Win32 would rewrite it or read it as a device, the shell would reach another entry.
            return Err(PathOpenDecision::Blocked);
        }
        match self.probe.attributes(&entry).map(entry_kind) {
            Some(LocalEntry::Directory) => self.push_entry(entry, name, PathKind::Directory),
            Some(LocalEntry::File) => self.push_entry(entry, name, PathKind::File),
            Some(LocalEntry::Link { directory }) => {
                self.dangling_blocks |= last;
                self.follow_link(entry, directory)
            }
            Some(LocalEntry::Refused) | None => Err(PathOpenDecision::Blocked),
        }
    }

    /// Hold an ordinary folder or file as the next part of the link-free path.
    fn push_entry(
        &mut self,
        entry: Probe::Handle,
        name: String,
        kind: PathKind,
    ) -> Result<(), PathOpenDecision> {
        self.chain.push(entry);
        self.names.push(name);
        self.kind = kind;
        Ok(())
    }

    /// Follow the held symlink or junction `link`. It is refused unless the walk stays within
    /// `MAX_LINK_HOPS`, the link's volume is not removable, and its target parses as a local form;
    /// otherwise the target's names are queued, then a check of the link's folder flag.
    fn follow_link(
        &mut self,
        link: Probe::Handle,
        directory: bool,
    ) -> Result<(), PathOpenDecision> {
        self.link_hops += 1;
        if self.link_hops > MAX_LINK_HOPS {
            // When: `link_hops` passes `MAX_LINK_HOPS`, refuse a loop or a chain longer than Windows resolves.
            return Err(PathOpenDecision::Blocked);
        }
        if self.removable {
            // When: `removable` marks the link's own volume, refuse it before reading its target; links join only fixed disks.
            return Err(PathOpenDecision::Blocked);
        }
        let raw_target = self.probe.link_target(&link);
        self.custody.push(link);
        let Some(target) = raw_target.as_deref().and_then(parse_link_target) else {
            // When: the target is unreadable or `parse_link_target` refuses its UNC, device, volume or rooted text.
            return Err(PathOpenDecision::Blocked);
        };
        self.pending.push(WalkStep::LinkEnd { directory });
        let names = match target {
            LinkTarget::Absolute { drive, names } => {
                self.enter_drive(drive, true)?;
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

    /// Climb `parent_steps` folders from the link's folder, by text on the held link-free path and
    /// never above the drive root; the folders climbed out of stay held.
    fn climb(&mut self, parent_steps: usize) -> Result<(), PathOpenDecision> {
        let Some(kept) = self.names.len().checked_sub(parent_steps) else {
            // When: `checked_sub` shows `parent_steps` would climb above the drive root, refuse rather than clamp.
            return Err(PathOpenDecision::Blocked);
        };
        self.custody.extend(self.chain.drain(kept + 1..));
        self.names.truncate(kept);
        self.kind = PathKind::Directory;
        Ok(())
    }

    /// End the walk. The link-free path is the verified drive letter and the names the walk opened,
    /// never the input text, and every held entry passes into one custody value.
    fn finish(mut self) -> HeldTarget<Probe::Handle> {
        let dispatch_path = format!("{}:\\{}", self.drive, self.names.join("\\"));
        self.custody.append(&mut self.chain);
        HeldTarget { kind: self.kind, dispatch_path, _custody: self.custody }
    }
}

/// Walk `path` under custody with `probe`, then apply the filename policy to the path's own final
/// name, the name the terminal showed; a drive root has none.
#[cfg(any(target_os = "windows", test))]
fn hold_windows_target<Probe: LocalLinkProbe>(
    path: &Path,
    probe: &mut Probe,
) -> Result<HeldTarget<Probe::Handle>, PathOpenDecision> {
    let Some(text) = path.to_str() else {
        // When: `to_str` fails, a non-UTF-8 path can be neither walked nor checked by filename policy.
        return Err(PathOpenDecision::Blocked);
    };
    let held = hold_local_target(text, probe)?;
    if held.kind == PathKind::Directory && path.parent().is_none() {
        // When: `kind` is `Directory` at the drive root, no final filename exists for extension policy.
        return Ok(held);
    }
    if windows_path_policy(path).is_blocked() {
        // When: `windows_path_policy` blocks the final name, refuse the target however the walk resolved it.
        return Err(PathOpenDecision::Blocked);
    }
    Ok(held)
}

/// Classify `path` with `probe`: walk it under custody, release every handle, and report the kind
/// the walk reached.
#[cfg(any(target_os = "windows", test))]
fn classify_windows_target_with<Probe: LocalLinkProbe>(
    path: &Path,
    probe: &mut Probe,
) -> PathOpenDecision {
    match hold_windows_target(path, probe) {
        Ok(held) => PathOpenDecision::Openable(held.kind),
        Err(decision) => decision,
    }
}

/// Walk `path` again at click time, holding every entry it opens, and run `action` on the
/// link-free path while they stay held; the handles close only after `action` returns. A target
/// whose walk no longer gives `expected_decision` is refused before `action` runs.
#[cfg(any(target_os = "windows", test))]
fn dispatch_held_target<Probe: LocalLinkProbe>(
    path: &Path,
    expected_decision: PathOpenDecision,
    probe: &mut Probe,
    action: impl FnOnce(&str) -> io::Result<()>,
) -> io::Result<()> {
    let held = hold_windows_target(path, probe)
        .ok()
        .filter(|held| PathOpenDecision::Openable(held.kind) == expected_decision)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::PermissionDenied, "changed or blocked Windows target")
        })?;
    let result = action(&held.dispatch_path);
    // Release the custody handles only now that the native call has returned.
    drop(held);
    result
}

/// Select the file at `path` in Explorer: walk it again under custody and pass Explorer the
/// link-free path while every part stays held.
#[cfg(target_os = "windows")]
pub(super) fn reveal_native_file(path: &Path) -> io::Result<()> {
    let expected_decision = PathOpenDecision::Openable(PathKind::File);
    dispatch_held_target(path, expected_decision, &mut NativeLinkProbe, select_in_explorer)
}

/// Navigate to the directory at `path`: walk it again under custody and pass the shell the
/// link-free path while every part stays held.
#[cfg(target_os = "windows")]
pub(super) fn open_native_path(path: &Path, expected_decision: PathOpenDecision) -> io::Result<()> {
    if expected_decision != PathOpenDecision::Openable(PathKind::Directory) {
        // When: `expected_decision` is not a directory, the navigation dispatcher must never launch a file.
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsupported Windows action"));
    }
    dispatch_held_target(path, expected_decision, &mut NativeLinkProbe, open_in_shell)
}

/// Select the file at the link-free `path` in its folder with `SHOpenFolderAndSelectItems`.
#[cfg(target_os = "windows")]
fn select_in_explorer(path: &str) -> io::Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName};
    let target = path.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
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

/// Open the directory at the link-free `path` with `ShellExecuteExW`, synchronously.
#[cfg(target_os = "windows")]
fn open_in_shell(path: &str) -> io::Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let verb = "open\0".encode_utf16().collect::<Vec<_>>();
    let target = path.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
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

/// The production probe: drive letters through `QueryDosDeviceW`, and entries through
/// `NtCreateFile`, relative to a held parent after the root, reading attributes, reparse data and
/// volume identity from the held handles themselves.
#[cfg(target_os = "windows")]
struct NativeLinkProbe;

#[cfg(target_os = "windows")]
impl LocalLinkProbe for NativeLinkProbe {
    type Handle = std::os::windows::io::OwnedHandle;

    fn dos_device(&mut self, letter: char) -> Option<String> {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::QueryDosDeviceW;
        let drive = format!("{letter}:").encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let mut targets = vec![0_u16; 1024];
        let written =
            // SAFETY: `drive` is a NUL-terminated UTF-16 device name and `targets` a writable buffer; both outlive the call.
            unsafe { QueryDosDeviceW(PCWSTR(drive.as_ptr()), Some(targets.as_mut_slice())) };
        let written = usize::try_from(written).ok().filter(|count| *count > 0)?;
        let first = targets.get(..written)?.split(|unit| *unit == 0).next()?;
        String::from_utf16(first).ok()
    }

    fn open_root(&mut self, device: &str) -> OpenOutcome<Self::Handle> {
        nt_open(None, &format!("{device}\\"))
    }

    fn open_child(&mut self, parent: &Self::Handle, name: &str) -> OpenOutcome<Self::Handle> {
        nt_open(Some(parent), name)
    }

    fn volume_device(&mut self, root: &Self::Handle) -> Option<VolumeDevice> {
        use std::os::windows::io::AsRawHandle;
        use windows::Wdk::Storage::FileSystem::{
            FileFsDeviceInformation, NtQueryVolumeInformationFile,
        };
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::IO::IO_STATUS_BLOCK;
        // `FILE_FS_DEVICE_INFORMATION` is two `u32` fields: the device type, then its characteristics.
        let mut information = [0_u32; 2];
        let length = u32::try_from(std::mem::size_of_val(&information)).ok()?;
        let mut status_block = IO_STATUS_BLOCK::default();
        let status =
            // SAFETY: `root` is a live handle; `status_block` and `information` are writable locals of the stated length.
            unsafe {
            NtQueryVolumeInformationFile(
                HANDLE(root.as_raw_handle()),
                &mut status_block,
                information.as_mut_ptr().cast(),
                length,
                FileFsDeviceInformation,
            )
        };
        (status.0 >= 0).then_some(VolumeDevice {
            device_type: information[0],
            characteristics: information[1],
        })
    }

    fn attributes(&mut self, entry: &Self::Handle) -> Option<EntryAttributes> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            FileAttributeTagInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_TAG_INFO,
        };
        let mut information = FILE_ATTRIBUTE_TAG_INFO { FileAttributes: 0, ReparseTag: 0 };
        let length = u32::try_from(std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>()).ok()?;
        let read =
            // SAFETY: `entry` is a live handle and `information` a writable `FILE_ATTRIBUTE_TAG_INFO` of `length` bytes.
            unsafe {
            GetFileInformationByHandleEx(
                HANDLE(entry.as_raw_handle()),
                FileAttributeTagInfo,
                std::ptr::from_mut(&mut information).cast(),
                length,
            )
        };
        read.ok()?;
        Some(EntryAttributes {
            attributes: information.FileAttributes,
            reparse_tag: information.ReparseTag,
        })
    }

    fn link_target(&mut self, link: &Self::Handle) -> Option<String> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::IO::DeviceIoControl;
        /// `FSCTL_GET_REPARSE_POINT`, which reads an entry's reparse data without following it.
        const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
        /// `MAXIMUM_REPARSE_DATA_BUFFER_SIZE`, the most reparse data an entry can hold.
        const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;
        let mut buffer = vec![0_u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
        let capacity = u32::try_from(buffer.len()).ok()?;
        let mut returned = 0_u32;
        let read =
            // SAFETY: `link` is a live synchronous handle; `buffer` holds `capacity` writable bytes and `returned` is a writable local.
            unsafe {
            DeviceIoControl(
                HANDLE(link.as_raw_handle()),
                FSCTL_GET_REPARSE_POINT,
                None,
                0,
                Some(buffer.as_mut_ptr().cast()),
                capacity,
                Some(std::ptr::from_mut(&mut returned)),
                None,
            )
        };
        read.ok()?;
        buffer.truncate(usize::try_from(returned).ok()?);
        decode_reparse_target(&buffer)
    }
}

/// `FILE_READ_DATA`, `FILE_LIST_DIRECTORY` on a folder: the right a readable part is held with.
const HOLD_READ_DATA: u32 = 0x0001;
/// `FILE_EXECUTE`, `FILE_TRAVERSE` on a folder: the right tried when reading a part is denied.
const HOLD_EXECUTE: u32 = 0x0020;
/// `FILE_READ_ATTRIBUTES | SYNCHRONIZE`: every attempt reads the entry's attributes and waits on
/// it synchronously.
const HOLD_BASE_ACCESS: u32 = 0x0080 | 0x0010_0000;
/// The access masks tried, in order, to hold one part. Each adds one read-class right, so the open
/// takes part in share-access checks, and none asks for write, delete, ownership or security
/// changes. Reading comes first because Explorer needs only that to select a file; executing
/// covers a folder whose listing is denied but whose traversal is allowed.
const HOLD_ACCESS_ATTEMPTS: [u32; 2] =
    [HOLD_READ_DATA | HOLD_BASE_ACCESS, HOLD_EXECUTE | HOLD_BASE_ACCESS];

/// The result of one open attempt with a single access mask.
enum OpenAttempt<Handle> {
    /// The access was denied; the next mask in `HOLD_ACCESS_ATTEMPTS` may still be allowed.
    AccessDenied,
    /// The attempt decided the entry: held, missing, or refused for any reason other than access.
    Settled(OpenOutcome<Handle>),
}

/// Hold one part with the first mask in `HOLD_ACCESS_ATTEMPTS` the entry allows, calling
/// `attempt` once per mask. Only a denied access moves on to the next mask; a part that every mask
/// is denied is refused.
fn open_with_read_class_access<Handle>(
    mut attempt: impl FnMut(u32) -> OpenAttempt<Handle>,
) -> OpenOutcome<Handle> {
    for access in HOLD_ACCESS_ATTEMPTS {
        if let OpenAttempt::Settled(outcome) = attempt(access) {
            // When: `attempt(access)` settled the entry, a later mask cannot change whether it exists or is held.
            return outcome;
        }
    }
    OpenOutcome::Refused
}

/// Open one entry with `NtCreateFile`: `name` in the held `parent`, or, without a parent, the NT
/// device root `name`. `OBJ_DONT_REPARSE` and `FILE_OPEN_REPARSE_POINT` keep the open from passing
/// through any link and remain set on every access-right attempt. A failed open reports `Missing`
/// for not-found statuses or `Refused` for other failures after the access-denied retry, never a
/// retry without the flags. Each attempt asks for one read-class right from `HOLD_ACCESS_ATTEMPTS`,
/// so the open takes part in share checks, and the share mode leaves out `FILE_SHARE_DELETE`, so the
/// entry cannot be renamed or deleted while it is held.
#[cfg(target_os = "windows")]
fn nt_open(
    parent: Option<&std::os::windows::io::OwnedHandle>,
    name: &str,
) -> OpenOutcome<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::core::PWSTR;
    use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows::Wdk::Storage::FileSystem::{
        NtCreateFile, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    };
    use windows::Win32::Foundation::{
        HANDLE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, STATUS_ACCESS_DENIED,
        STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND, UNICODE_STRING,
    };
    use windows::Win32::Storage::FileSystem::{
        FILE_ACCESS_RIGHTS, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use windows::Win32::System::IO::IO_STATUS_BLOCK;
    let mut units = name.encode_utf16().collect::<Vec<_>>();
    let Some(length) = units.len().checked_mul(2).and_then(|bytes| u16::try_from(bytes).ok())
    else {
        // When: `units` need more bytes than `u16::try_from` allows, no `UNICODE_STRING` can count `name`, so no entry carries it.
        return OpenOutcome::Missing;
    };
    let object_name =
        UNICODE_STRING { Length: length, MaximumLength: length, Buffer: PWSTR(units.as_mut_ptr()) };
    let attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(std::mem::size_of::<OBJECT_ATTRIBUTES>()).unwrap_or(u32::MAX),
        RootDirectory: parent
            .map_or(HANDLE(std::ptr::null_mut()), |held| HANDLE(held.as_raw_handle())),
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    open_with_read_class_access(|access| {
        let mut handle = HANDLE(std::ptr::null_mut());
        let mut status_block = IO_STATUS_BLOCK::default();
        let status =
        // SAFETY: `attributes`, `object_name`, `units` and `parent` outlive this synchronous call; `handle` and `status_block` are writable locals.
        unsafe {
        NtCreateFile(
            &mut handle,
            FILE_ACCESS_RIGHTS(access),
            &attributes,
            &mut status_block,
            None,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            None,
            0,
        )
    };
        if status == STATUS_ACCESS_DENIED {
            // When: `status` is STATUS_ACCESS_DENIED for this `access`, the next read-class right may still be allowed.
            return OpenAttempt::AccessDenied;
        }
        if status == STATUS_OBJECT_NAME_NOT_FOUND || status == STATUS_OBJECT_PATH_NOT_FOUND {
            // When: `status` reports no object at `name`, nothing exists there to hold.
            return OpenAttempt::Settled(OpenOutcome::Missing);
        }
        if status.0 < 0 {
            // When: `status` is still failing, access-denied and not-found statuses were handled above; refuse this remaining failure.
            return OpenAttempt::Settled(OpenOutcome::Refused);
        }
        let held =
        // SAFETY: `NtCreateFile` succeeded, so `handle` is an open handle that no other value owns.
        unsafe { OwnedHandle::from_raw_handle(handle.0) };
        OpenAttempt::Settled(OpenOutcome::Held(held))
    })
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod windows_tests;
