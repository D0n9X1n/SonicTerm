//! Windows path-target tests. The custody-walk tests run on every OS against an in-memory volume
//! table that records every call and every handle release in order; the symlink, junction,
//! drive-letter and custody tests run only on Windows against real entries; and ignored native
//! probes let an external driver check Explorer selection and structural-path interaction in real
//! windows.

pub(super) use std::cell::RefCell;
pub(super) use std::collections::{BTreeMap, BTreeSet};

use super::*;
#[cfg(target_os = "windows")]
pub(super) use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

// `FileFsDeviceInformation` device types the tests report, besides `FILE_DEVICE_DISK`.
pub(super) const FILE_DEVICE_CD_ROM: u32 = 0x2;
pub(super) const FILE_DEVICE_NETWORK_FILE_SYSTEM: u32 = 0x14;
pub(super) const FILE_DEVICE_VIRTUAL_DISK: u32 = 0x24;

// Reparse tags the tests report, besides symlinks and junctions.
pub(super) const IO_REPARSE_TAG_CLOUD: u32 = 0x9000_001A;
pub(super) const IO_REPARSE_TAG_APPEXECLINK: u32 = 0x8000_001B;
pub(super) const IO_REPARSE_TAG_LX_SYMLINK: u32 = 0xA000_001D;

/// The attributes of an ordinary folder in the fake volume table.
pub(super) const FOLDER_ENTRY: EntryAttributes =
    EntryAttributes { attributes: 0x10, reparse_tag: 0 };

/// The attributes of an ordinary file in the fake volume table.
pub(super) const FILE_ENTRY: EntryAttributes = EntryAttributes { attributes: 0x20, reparse_tag: 0 };

/// The device a local fixed disk reports.
pub(super) const FIXED_DISK: VolumeDevice =
    VolumeDevice { device_type: FILE_DEVICE_DISK, characteristics: 0 };

/// A removable disk, such as a USB stick.
pub(super) const REMOVABLE_DISK: VolumeDevice =
    VolumeDevice { device_type: FILE_DEVICE_DISK, characteristics: FILE_REMOVABLE_MEDIA };

/// A network file system, as a mapped share reports.
pub(super) const NETWORK_SHARE: VolumeDevice = VolumeDevice {
    device_type: FILE_DEVICE_NETWORK_FILE_SYSTEM,
    characteristics: FILE_REMOTE_DEVICE,
};

/// A held entry in the fake volume table, named by its full path so the log can show which entry a
/// call used. Dropping it records the release.
pub(super) struct FakeHandle {
    path: String,
    log: std::rc::Rc<RefCell<Vec<String>>>,
}

// Lifecycle: dropping a `FakeHandle` records `drop` with its path, so tests see when custody ends.
impl Drop for FakeHandle {
    fn drop(&mut self) {
        self.log.borrow_mut().push(format!("drop {}", self.path));
    }
}

/// An in-memory set of Windows volumes for custody-walk tests. It records every call the walk makes
/// and every handle it releases, in order, so a test can prove what the walk opened, when it let
/// go, and what it never touched. A letter whose root folder exists is a local fixed disk, reached
/// through a hard-disk volume numbered by the letter, unless a test says otherwise.
#[derive(Default)]
pub(super) struct FakeVolumes {
    entries: BTreeMap<String, EntryAttributes>,
    targets: BTreeMap<String, String>,
    denied: BTreeSet<String>,
    devices: BTreeMap<char, Option<String>>,
    volumes: BTreeMap<char, VolumeDevice>,
    log: std::rc::Rc<RefCell<Vec<String>>>,
}

impl FakeVolumes {
    fn folders(mut self, paths: &[&str]) -> Self {
        for path in paths {
            self.entries.insert((*path).to_string(), FOLDER_ENTRY);
        }
        self
    }

    fn files(mut self, paths: &[&str]) -> Self {
        for path in paths {
            self.entries.insert((*path).to_string(), FILE_ENTRY);
        }
        self
    }

    /// A reparse point that is neither a symlink nor a junction, such as a cloud-file placeholder.
    fn refused(mut self, path: &str) -> Self {
        let placeholder = EntryAttributes { attributes: 0x420, reparse_tag: IO_REPARSE_TAG_CLOUD };
        self.entries.insert(path.to_string(), placeholder);
        self
    }

    /// An entry that exists but cannot be opened, as when access to it is denied.
    fn denied(mut self, path: &str) -> Self {
        self.denied.insert(path.to_string());
        self
    }

    /// A symlink, a folder symlink when `directory` is set, whose reparse data names `target`.
    fn link(mut self, path: &str, directory: bool, target: &str) -> Self {
        let folder_bit = if directory { 0x10 } else { 0 };
        let link =
            EntryAttributes { attributes: 0x400 | folder_bit, reparse_tag: IO_REPARSE_TAG_SYMLINK };
        self.entries.insert(path.to_string(), link);
        self.targets.insert(path.to_string(), target.to_string());
        self
    }

    /// Report `device` as the first `QueryDosDeviceW` target of `letter`, or a failed query for `None`.
    fn device(mut self, letter: char, device: Option<&str>) -> Self {
        self.devices.insert(letter, device.map(str::to_string));
        self
    }

    /// Report `volume` for the root of `letter`.
    fn volume(mut self, letter: char, volume: VolumeDevice) -> Self {
        self.volumes.insert(letter, volume);
        self
    }

    /// The target `QueryDosDeviceW` reports for `letter`: a test's choice, else a hard-disk volume
    /// numbered by the letter when its root folder exists, else a failed query.
    fn device_of(&self, letter: char) -> Option<String> {
        if let Some(device) = self.devices.get(&letter) {
            return device.clone();
        }
        let number = u32::from(letter) - u32::from('A');
        let root = format!("{letter}:\\");
        self.entries.contains_key(&root).then(|| format!(r"\Device\HarddiskVolume{number}"))
    }

    fn record(&self, line: String) {
        self.log.borrow_mut().push(line);
    }

    fn handle(&self, path: String) -> FakeHandle {
        FakeHandle { path, log: std::rc::Rc::clone(&self.log) }
    }

    /// Walk `path` and release every handle, reporting the kind the walk reached.
    fn resolve(&mut self, path: &str) -> Result<PathKind, PathOpenDecision> {
        hold_local_target(path, self).map(|held| held.kind)
    }

    /// Remove and return the log so far.
    fn take_log(&self) -> Vec<String> {
        self.log.borrow_mut().drain(..).collect()
    }

    /// The logged calls of `kind`, such as `link_target`, without the call name.
    fn calls(&self, kind: &str) -> Vec<String> {
        let prefix = format!("{kind} ");
        let log = self.log.borrow();
        log.iter().filter_map(|line| line.strip_prefix(&prefix).map(str::to_string)).collect()
    }

    /// The `open_root` and `open_child` calls, in order.
    fn opened(&self) -> Vec<String> {
        self.log.borrow().iter().filter(|line| line.starts_with("open_")).cloned().collect()
    }

    /// Whether any logged call on an entry or a volume, as opposed to a namespace query or a
    /// release, named `fragment`.
    fn touched(&self, fragment: &str) -> bool {
        self.log.borrow().iter().any(|line| {
            !line.starts_with("dos_device ")
                && !line.starts_with("drop ")
                && line.contains(fragment)
        })
    }
}

impl LocalLinkProbe for FakeVolumes {
    type Handle = FakeHandle;

    fn dos_device(&mut self, letter: char) -> Option<String> {
        self.record(format!("dos_device {letter}"));
        self.device_of(letter)
    }

    fn open_root(&mut self, device: &str) -> OpenOutcome<FakeHandle> {
        self.record(format!("open_root {device}"));
        let letter = ('A'..='Z').find(|letter| self.device_of(*letter).as_deref() == Some(device));
        match letter {
            Some(letter) => OpenOutcome::Held(self.handle(format!("{letter}:\\"))),
            None => OpenOutcome::Missing,
        }
    }

    fn open_child(&mut self, parent: &FakeHandle, name: &str) -> OpenOutcome<FakeHandle> {
        self.record(format!("open_child {} {name}", parent.path));
        let path = if parent.path.ends_with('\\') {
            format!("{}{name}", parent.path)
        } else {
            format!("{}\\{name}", parent.path)
        };
        if self.denied.contains(&path) {
            OpenOutcome::Refused
        } else if self.entries.contains_key(&path) {
            OpenOutcome::Held(self.handle(path))
        } else {
            OpenOutcome::Missing
        }
    }

    fn volume_device(&mut self, root: &FakeHandle) -> Option<VolumeDevice> {
        self.record(format!("volume_device {}", root.path));
        let letter = root.path.chars().next()?;
        Some(self.volumes.get(&letter).copied().unwrap_or(FIXED_DISK))
    }

    fn attributes(&mut self, entry: &FakeHandle) -> Option<EntryAttributes> {
        self.record(format!("attributes {}", entry.path));
        self.entries.get(&entry.path).copied()
    }

    fn link_target(&mut self, link: &FakeHandle) -> Option<String> {
        self.record(format!("link_target {}", link.path));
        self.targets.get(&link.path).cloned()
    }
}

/// A `REPARSE_DATA_BUFFER` as `FSCTL_GET_REPARSE_POINT` returns it: the tag, the data length, a
/// reserved word, the name offsets and lengths, the symlink flags when `flags` is given, then the
/// substitute and print names, each NUL-terminated.
pub(super) fn reparse_buffer(
    tag: u32,
    flags: Option<u32>,
    substitute: &str,
    print: &str,
) -> Vec<u8> {
    let encode = |text: &str| {
        text.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect::<Vec<_>>()
    };
    let substitute_bytes = encode(substitute);
    let print_bytes = encode(print);
    let substitute_length = u16::try_from(substitute_bytes.len() - 2).unwrap();
    let print_offset = u16::try_from(substitute_bytes.len()).unwrap();
    let print_length = u16::try_from(print_bytes.len() - 2).unwrap();
    let mut data = [0, substitute_length, print_offset, print_length]
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    if let Some(flags) = flags {
        data.extend(flags.to_le_bytes());
    }
    data.extend(substitute_bytes);
    data.extend(print_bytes);
    let mut buffer = tag.to_le_bytes().to_vec();
    buffer.extend(u16::try_from(data.len()).unwrap().to_le_bytes());
    buffer.extend([0_u8; 2]);
    buffer.extend(data);
    buffer
}

/// A path without links resolves its drive letter once, opens the root through the NT device path,
/// and opens each later part by one name below its held parent, never by a path; `.` and `..`
/// resolve by text first. Every handle stays held until the walk ends, then all are released.
#[test]
pub(super) fn plain_paths_open_each_part_below_its_held_parent() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\work\sub", r"E:\"])
        .files(&[r"C:\work\notes.txt", r"E:\notes.txt"])
        .volume('E', REMOVABLE_DISK);
    assert_eq!(volumes.resolve(r"C:\work\notes.txt"), Ok(PathKind::File));
    assert_eq!(
        volumes.take_log(),
        [
            "dos_device C",
            r"open_root \Device\HarddiskVolume2",
            r"volume_device C:\",
            r"attributes C:\",
            r"open_child C:\ work",
            r"attributes C:\work",
            r"open_child C:\work notes.txt",
            r"attributes C:\work\notes.txt",
            r"drop C:\",
            r"drop C:\work",
            r"drop C:\work\notes.txt",
        ]
    );
    // `.` and `..` resolve by text before any call, so only real names are ever opened.
    assert_eq!(volumes.resolve("c:/work/./sub/../notes.txt"), Ok(PathKind::File));
    assert_eq!(volumes.calls("open_child"), [r"C:\ work", r"C:\work notes.txt"]);
    volumes.take_log();
    assert_eq!(volumes.resolve(r"C:\work"), Ok(PathKind::Directory));
    assert_eq!(volumes.resolve(r"C:\"), Ok(PathKind::Directory));
    // A removable disk may start a walk; only following a link requires a fixed disk.
    assert_eq!(volumes.resolve(r"E:\notes.txt"), Ok(PathKind::File));
    assert_eq!(volumes.resolve(r"C:\work\gone.txt"), Err(PathOpenDecision::Missing));
    assert_eq!(volumes.resolve(r"C:\gone\notes.txt"), Err(PathOpenDecision::Missing));
    // Windows reports a path through a file as not found, so it stays missing.
    assert_eq!(volumes.resolve(r"C:\work\notes.txt\more"), Err(PathOpenDecision::Missing));
    // A name holding `:` names a stream, never an entry, so it is missing without being opened.
    assert_eq!(volumes.resolve(r"C:\work\notes.txt:12"), Err(PathOpenDecision::Missing));
    assert!(!volumes.touched("notes.txt:12"));
    assert!(volumes.calls("link_target").is_empty());
    // Each open after the root names one component below a held parent, never a path.
    assert!(volumes.calls("open_child").iter().all(|call| {
        let (_parent, name) = call.rsplit_once(' ').unwrap();
        !name.contains(['\\', '/'])
    }));
    assert_eq!(volumes.calls("open_root").len(), volumes.calls("dos_device").len());
}

/// A drive letter whose first `QueryDosDeviceW` target is not exactly a hard-disk volume is refused
/// right after that namespace query, before any other call: network redirectors, `Mup`, a `subst`
/// folder, an optical or RAM drive, an empty target and a failed query, with or without links.
#[test]
pub(super) fn drive_letters_that_are_not_hard_disk_volumes_make_no_call_after_the_query() {
    let devices = [
        Some(r"\Device\LanmanRedirector\;Z:0000000000012345\host\share"),
        Some(r"\Device\Mup\;LanmanRedirector\;Z:0000000000012345\host\share"),
        Some(r"\Device\Mup"),
        Some(r"\Device\WebDavRedirector\;Z:0000000000012345\host\share"),
        Some(r"\Device\RdpDr\;Z:1\tsclient\C"),
        Some(r"\??\C:\work"),
        Some(r"\Device\CdRom0"),
        Some(r"\Device\Ramdisk0"),
        Some(""),
        None,
    ];
    for device in devices {
        let mut volumes = FakeVolumes::default()
            .folders(&[r"C:\", r"C:\work", r"Z:\", r"Z:\share"])
            .files(&[r"C:\work\notes.txt", r"Z:\share\notes.txt"])
            .link(r"Z:\share\link.txt", false, r"\??\C:\work\notes.txt")
            .device('Z', device);
        for path in [r"Z:\share\notes.txt", r"Z:\share\link.txt", r"Z:\"] {
            assert_eq!(volumes.resolve(path), Err(PathOpenDecision::Blocked), "{device:?} {path}");
            assert_eq!(volumes.take_log(), ["dos_device Z"], "{device:?} {path}");
        }
    }
    let mut unmapped = FakeVolumes::default().folders(&[r"C:\"]);
    assert_eq!(unmapped.resolve(r"Y:\notes.txt"), Err(PathOpenDecision::Blocked));
    assert_eq!(unmapped.take_log(), ["dos_device Y"]);
}

/// A volume whose root reports a network file system, a remote disk, an optical drive or a virtual
/// disk is refused as soon as its root reports it, before any part below the root is opened.
#[test]
pub(super) fn remote_and_non_disk_volumes_are_refused_before_any_part_is_opened() {
    for volume in [
        NETWORK_SHARE,
        VolumeDevice { device_type: FILE_DEVICE_DISK, characteristics: FILE_REMOTE_DEVICE },
        VolumeDevice { device_type: FILE_DEVICE_CD_ROM, characteristics: FILE_REMOVABLE_MEDIA },
        VolumeDevice { device_type: FILE_DEVICE_VIRTUAL_DISK, characteristics: 0 },
    ] {
        let mut fake = FakeVolumes::default()
            .folders(&[r"Z:\", r"Z:\share"])
            .files(&[r"Z:\share\notes.txt"])
            .volume('Z', volume);
        let decision = fake.resolve(r"Z:\share\notes.txt");
        assert_eq!(decision, Err(PathOpenDecision::Blocked), "{volume:?}");
        assert_eq!(
            fake.take_log(),
            [
                "dos_device Z",
                r"open_root \Device\HarddiskVolume25",
                r"volume_device Z:\",
                r"drop Z:\",
            ],
            "{volume:?}"
        );
    }
}

/// A file symlink on a local fixed disk is followed whether its reparse data names an NT drive path
/// or a path relative to the link's folder.
#[test]
pub(super) fn file_links_on_local_fixed_disks_resolve_to_their_targets() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\real"])
        .files(&[r"C:\real\notes.txt"])
        .link(r"C:\work\nt.txt", false, r"\??\C:\real\notes.txt")
        .link(r"C:\work\relative.txt", false, r"..\real\notes.txt");
    for link in [r"C:\work\nt.txt", r"C:\work\relative.txt"] {
        assert_eq!(volumes.resolve(link), Ok(PathKind::File), "{link}");
    }
    assert_eq!(volumes.calls("link_target"), [r"C:\work\nt.txt", r"C:\work\relative.txt"]);
    assert!(volumes.calls("dos_device").iter().all(|letter| letter == "C"));
}

/// A junction partway along a path is replaced by its target before the walk goes deeper: the walk
/// restarts at the target drive's root and opens every later part below an ordinary held folder,
/// never through the junction, and releases every handle it opened, the junction's included, at
/// the end.
#[test]
pub(super) fn junction_ancestors_are_replaced_by_their_targets_before_descending() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\real", r"C:\real\sub"])
        .files(&[r"C:\real\sub\notes.txt"])
        .link(r"C:\work\junction", true, r"\??\C:\real");
    assert_eq!(volumes.resolve(r"C:\work\junction\sub\notes.txt"), Ok(PathKind::File));
    assert_eq!(
        volumes.opened(),
        [
            r"open_root \Device\HarddiskVolume2",
            r"open_child C:\ work",
            r"open_child C:\work junction",
            r"open_root \Device\HarddiskVolume2",
            r"open_child C:\ real",
            r"open_child C:\real sub",
            r"open_child C:\real\sub notes.txt",
        ]
    );
    assert_eq!(volumes.calls("link_target"), [r"C:\work\junction"]);
    assert_eq!(
        volumes.calls("drop"),
        [
            r"C:\work\junction",
            r"C:\",
            r"C:\work",
            r"C:\",
            r"C:\real",
            r"C:\real\sub",
            r"C:\real\sub\notes.txt",
        ]
    );
    volumes.take_log();
    assert_eq!(volumes.resolve(r"C:\work\junction"), Ok(PathKind::Directory));
}

/// A link whose target is a UNC share, a device path, a volume-GUID mount folder, or a bare or
/// `\\?\` drive path is refused right after its reparse data is read, with no call on the target,
/// whether the link is the final name or a folder along the path.
#[test]
pub(super) fn links_to_unc_shares_devices_and_volume_mounts_are_refused_before_any_call_on_the_target(
) {
    for target in [
        r"\\host\share\notes.txt",
        r"\\?\UNC\host\share\notes.txt",
        r"\??\UNC\host\share\notes.txt",
        r"\\.\pipe\host",
        r"\??\GLOBALROOT\Device\Mup\host\share",
        r"\\?\GLOBALROOT\Device\Mup\host\share",
        r"\Device\Mup\host\share",
        r"\??\Volume{00000000-0000-0000-0000-000000000000}\host",
        r"\\?\C:\host",
        r"C:\host",
    ] {
        let mut volumes = FakeVolumes::default()
            .folders(&[r"C:\", r"C:\work", r"C:\host"])
            .link(r"C:\work\file-link.txt", false, target)
            .link(r"C:\work\folder-link", true, target);
        for path in [r"C:\work\file-link.txt", r"C:\work\folder-link\notes.txt"] {
            assert_eq!(volumes.resolve(path), Err(PathOpenDecision::Blocked), "{target} {path}");
            let log = volumes.take_log();
            let read = log.iter().position(|line| line.starts_with("link_target ")).unwrap();
            assert!(log[read + 1..].iter().all(|line| line.starts_with("drop ")), "{target}");
            assert!(!log.iter().any(|line| line.contains("host")), "{target} {path}");
        }
    }
}

/// A link onto a drive that is not a local fixed disk is refused before anything below that drive's
/// root is opened: a `subst` or unmapped letter right after the namespace query, a remote or
/// removable volume as soon as its root reports it. The same links resolve onto a local fixed disk.
#[test]
pub(super) fn links_onto_drives_that_are_not_local_fixed_disks_are_refused_before_opening_below_their_root(
) {
    fn volumes_with(configure: fn(FakeVolumes) -> FakeVolumes) -> FakeVolumes {
        configure(
            FakeVolumes::default()
                .folders(&[r"C:\", r"C:\work", r"Z:\", r"Z:\share"])
                .files(&[r"Z:\share\notes.txt"])
                .link(r"C:\work\mapped.txt", false, r"\??\Z:\share\notes.txt")
                .link(r"C:\work\mapped-folder", true, r"\??\Z:\share"),
        )
    }
    let refusals: [(fn(FakeVolumes) -> FakeVolumes, bool); 4] = [
        (|volumes: FakeVolumes| volumes.device('Z', Some(r"\??\C:\work")), false),
        (|volumes: FakeVolumes| volumes.device('Z', None), false),
        (|volumes: FakeVolumes| volumes.volume('Z', NETWORK_SHARE), true),
        (|volumes: FakeVolumes| volumes.volume('Z', REMOVABLE_DISK), true),
    ];
    for (configure, root_opened) in refusals {
        let mut volumes = volumes_with(configure);
        for path in [r"C:\work\mapped.txt", r"C:\work\mapped-folder\notes.txt"] {
            assert_eq!(volumes.resolve(path), Err(PathOpenDecision::Blocked), "{path}");
            let log = volumes.take_log();
            let target_calls = log
                .iter()
                .skip_while(|line| !line.starts_with("link_target "))
                .skip(1)
                .filter(|line| !line.starts_with("drop "))
                .cloned()
                .collect::<Vec<_>>();
            let expected: &[&str] = if root_opened {
                &["dos_device Z", r"open_root \Device\HarddiskVolume25", r"volume_device Z:\"]
            } else {
                &["dos_device Z"]
            };
            assert_eq!(target_calls, expected, "{path}");
        }
    }
    let mut fixed = volumes_with(|volumes: FakeVolumes| volumes);
    assert_eq!(fixed.resolve(r"C:\work\mapped.txt"), Ok(PathKind::File));
    assert_eq!(fixed.resolve(r"C:\work\mapped-folder\notes.txt"), Ok(PathKind::File));
}

/// A link held on a removable disk is never followed, not even toward a local fixed disk, and its
/// target is never read; a plain file beside it still resolves, since a removable disk starts a walk.
#[test]
pub(super) fn links_held_on_removable_disks_are_refused_before_their_targets_are_read() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\real", r"E:\", r"E:\share"])
        .files(&[r"C:\real\notes.txt", r"E:\share\notes.txt"])
        .link(r"E:\share\link.txt", false, r"\??\C:\real\notes.txt")
        .volume('E', REMOVABLE_DISK);
    assert_eq!(volumes.resolve(r"E:\share\link.txt"), Err(PathOpenDecision::Blocked));
    assert!(volumes.calls("link_target").is_empty());
    assert!(!volumes.touched(r"C:\"));
    assert_eq!(volumes.resolve(r"E:\share\notes.txt"), Ok(PathKind::File));
}

/// A reparse point other than a symlink or junction, such as a cloud-file placeholder, and an entry
/// that cannot be opened are refused both as the final name and as a folder along the path.
#[test]
pub(super) fn other_reparse_points_and_unopenable_entries_are_refused() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work"])
        .refused(r"C:\work\placeholder.txt")
        .refused(r"C:\work\cloud")
        .denied(r"C:\work\private");
    for path in [
        r"C:\work\placeholder.txt",
        r"C:\work\cloud\notes.txt",
        r"C:\work\private",
        r"C:\work\private\notes.txt",
    ] {
        assert_eq!(volumes.resolve(path), Err(PathOpenDecision::Blocked), "{path}");
    }
    assert!(volumes.calls("link_target").is_empty());
}

/// A dangling final link is refused, as on macOS and Linux, while a missing name below a link
/// stays missing, as it does below a folder, so a shorter candidate path can still be tried.
#[test]
pub(super) fn dangling_final_links_block_while_missing_names_below_links_stay_missing() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\real"])
        .link(r"C:\work\dangling.txt", false, r"\??\C:\real\gone.txt")
        .link(r"C:\work\gone-folder", true, r"\??\C:\gone")
        .link(r"C:\work\real-folder", true, r"\??\C:\real");
    assert_eq!(volumes.resolve(r"C:\work\dangling.txt"), Err(PathOpenDecision::Blocked));
    assert_eq!(volumes.resolve(r"C:\work\gone-folder"), Err(PathOpenDecision::Blocked));
    let missing = Err(PathOpenDecision::Missing);
    assert_eq!(volumes.resolve(r"C:\work\gone-folder\notes.txt"), missing);
    assert_eq!(volumes.resolve(r"C:\work\real-folder\gone.txt"), missing);
}

/// A link to itself, a loop between two folder links, and a chain one hop longer than
/// `MAX_LINK_HOPS` are refused; a chain of exactly `MAX_LINK_HOPS` links still resolves.
#[test]
pub(super) fn link_loops_and_overlong_chains_are_refused() {
    let chain = |hop_count: usize| {
        let mut volumes =
            FakeVolumes::default().folders(&[r"C:\", r"C:\work"]).files(&[r"C:\work\end.txt"]);
        for hop in 0..hop_count {
            let target = if hop + 1 == hop_count {
                "end.txt".to_string()
            } else {
                format!("{}.txt", hop + 1)
            };
            volumes = volumes.link(&format!(r"C:\work\{hop}.txt"), false, &target);
        }
        volumes
    };
    assert_eq!(chain(MAX_LINK_HOPS).resolve(r"C:\work\0.txt"), Ok(PathKind::File));
    let overlong = chain(MAX_LINK_HOPS + 1).resolve(r"C:\work\0.txt");
    assert_eq!(overlong, Err(PathOpenDecision::Blocked));
    let mut loops = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work"])
        .link(r"C:\work\self.txt", false, "self.txt")
        .link(r"C:\work\ping", true, "pong")
        .link(r"C:\work\pong", true, "ping");
    assert_eq!(loops.resolve(r"C:\work\self.txt"), Err(PathOpenDecision::Blocked));
    assert_eq!(loops.resolve(r"C:\work\ping\notes.txt"), Err(PathOpenDecision::Blocked));
}

/// A folder link must reach a folder and a file link a file; a link whose target has the other
/// kind is refused rather than guessed, since Explorer treats a link by its own folder flag.
#[test]
pub(super) fn links_must_reach_the_kind_their_own_folder_flag_promises() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\real"])
        .files(&[r"C:\real\notes.txt"])
        .link(r"C:\work\folder-to-file", true, r"\??\C:\real\notes.txt")
        .link(r"C:\work\file-to-folder.txt", false, r"\??\C:\real");
    let blocked = Err(PathOpenDecision::Blocked);
    assert_eq!(volumes.resolve(r"C:\work\folder-to-file"), blocked);
    assert_eq!(volumes.resolve(r"C:\work\file-to-folder.txt"), blocked);
    assert_eq!(volumes.resolve(r"C:\work\file-to-folder.txt\notes.txt"), blocked);
}

/// A relative target climbs from the link's folder with leading `..` names, but a target that
/// would climb above the drive root is refused rather than clamped to it.
#[test]
pub(super) fn relative_targets_climb_from_the_link_folder_but_not_above_the_root() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\work\sub"])
        .files(&[r"C:\notes.txt"])
        .link(r"C:\work\sub\up.txt", false, r"..\..\notes.txt")
        .link(r"C:\work\sub\over.txt", false, r"..\..\..\notes.txt");
    assert_eq!(volumes.resolve(r"C:\work\sub\up.txt"), Ok(PathKind::File));
    assert_eq!(volumes.resolve(r"C:\work\sub\over.txt"), Err(PathOpenDecision::Blocked));
}

/// A chain of links whose targets are long paths is refused once the walk has opened
/// `MAX_WALK_ENTRIES` entries, well before `MAX_LINK_HOPS`; the same chain resolves when shorter.
#[test]
pub(super) fn long_link_chains_stop_at_the_open_bound() {
    // Each hop names an absolute target 100 folders deep, so it costs 102 opens.
    let deep = format!("C:{}", r"\deep".repeat(100));
    let chain = |link_count: usize| {
        let mut folders = vec![r"C:\".to_string()];
        let mut folder = "C:".to_string();
        for _ in 0..100 {
            folder.push_str(r"\deep");
            folders.push(folder.clone());
        }
        let folder_names = folders.iter().map(String::as_str).collect::<Vec<_>>();
        let end = format!(r"{deep}\end.txt");
        let mut volumes = FakeVolumes::default().folders(&folder_names).files(&[end.as_str()]);
        for link_index in 0..link_count {
            let target = if link_index + 1 == link_count {
                format!(r"\??\{end}")
            } else {
                format!(r"\??\{deep}\{}.txt", link_index + 1)
            };
            volumes = volumes.link(&format!(r"{deep}\{link_index}.txt"), false, &target);
        }
        volumes
    };
    let first_link = format!(r"{deep}\0.txt");
    let mut long = chain(12);
    assert_eq!(long.resolve(&first_link), Err(PathOpenDecision::Blocked));
    assert_eq!(long.opened().len(), MAX_WALK_ENTRIES);
    assert!(long.calls("link_target").len() < MAX_LINK_HOPS);
    assert_eq!(chain(4).resolve(&first_link), Ok(PathKind::File));
}

/// Only a drive-absolute path is walked: UNC, verbatim, device, rooted, drive-relative and
/// relative paths are refused before any call.
#[test]
pub(super) fn only_drive_absolute_paths_are_walked() {
    let mut volumes = FakeVolumes::default().folders(&[r"C:\"]);
    for path in [
        r"\\host\share\notes.txt",
        r"\\?\C:\notes.txt",
        r"\\.\pipe\host",
        r"\notes.txt",
        "C:notes.txt",
        "notes.txt",
        "",
    ] {
        assert_eq!(volumes.resolve(path), Err(PathOpenDecision::Blocked), "{path}");
    }
    assert!(volumes.take_log().is_empty());
}

/// An existing entry whose name Win32 would rewrite, such as a trailing dot or space, or read as a
/// DOS device, such as `LPT1`, is refused wherever it appears, since the shell given the walked
/// path would reach another entry; such a name that does not exist stays missing.
#[test]
pub(super) fn names_the_shell_would_read_as_another_entry_are_refused() {
    let mut volumes = FakeVolumes::default().folders(&[r"C:\", r"C:\work.", r"C:\work"]).files(&[
        r"C:\work.\notes.txt",
        r"C:\work\notes.txt ",
        r"C:\work\LPT1",
        r"C:\work\nul.txt",
    ]);
    let blocked = Err(PathOpenDecision::Blocked);
    assert_eq!(volumes.resolve(r"C:\work.\notes.txt"), blocked);
    assert_eq!(volumes.resolve(r"C:\work\notes.txt "), blocked);
    assert_eq!(volumes.resolve(r"C:\work\LPT1"), blocked);
    assert_eq!(volumes.resolve(r"C:\work\nul.txt"), blocked);
    let missing = Err(PathOpenDecision::Missing);
    assert_eq!(volumes.resolve(r"C:\gone.\notes.txt"), missing);
    assert_eq!(volumes.resolve(r"C:\work\con.txt"), missing);
}

/// Dispatch walks the path again and hands the action the link-free path built from the verified
/// drive letter and the names the walk opened, not the input text; every handle the walk opened,
/// the link's included, stays held until the action returns and is released only after it.
#[test]
pub(super) fn dispatch_passes_the_walked_path_while_every_handle_is_held() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\real"])
        .files(&[r"C:\real\notes.txt"])
        .link(r"C:\work\link.txt", false, r"..\real\notes.txt");
    let log = std::rc::Rc::clone(&volumes.log);
    let mut received = None;
    let expected_decision = PathOpenDecision::Openable(PathKind::File);
    let path = Path::new(r"c:\work\.\link.txt");
    dispatch_held_target(path, expected_decision, &mut volumes, |walked| {
        log.borrow_mut().push(format!("action {walked}"));
        received = Some(walked.to_string());
        Ok(())
    })
    .unwrap();
    assert_eq!(received.as_deref(), Some(r"C:\real\notes.txt"));
    let events = volumes.take_log();
    let action = events.iter().position(|line| line.starts_with("action ")).unwrap();
    assert!(events[..action].iter().all(|line| !line.starts_with("drop ")));
    assert_eq!(
        events[action + 1..],
        [
            r"drop C:\work\link.txt",
            r"drop C:\work",
            r"drop C:\",
            r"drop C:\real",
            r"drop C:\real\notes.txt",
        ]
    );
}

/// Dispatch refuses a target whose walk no longer gives the authorized decision before the action
/// runs, and a refused dispatch or a finished classification leaves no handle held.
#[test]
pub(super) fn dispatch_refuses_a_changed_target_and_releases_every_handle() {
    let mut volumes =
        FakeVolumes::default().folders(&[r"C:\", r"C:\work"]).files(&[r"C:\work\notes.txt"]);
    let path = Path::new(r"C:\work\notes.txt");
    let classified = classify_windows_target_with(path, &mut volumes);
    assert_eq!(classified, PathOpenDecision::Openable(PathKind::File));
    let expected_decision = PathOpenDecision::Openable(PathKind::Directory);
    let refused = dispatch_held_target(path, expected_decision, &mut volumes, |_| {
        unreachable!("a refused target never reaches the action")
    });
    assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    let events = volumes.take_log();
    let opens = events.iter().filter(|line| line.starts_with("open_")).count();
    assert_eq!(opens, 6);
    assert_eq!(events.iter().filter(|line| line.starts_with("drop ")).count(), opens);
    assert!(events.last().is_some_and(|line| line.starts_with("drop ")));
}

/// Every open attempt asks for exactly one read-class right plus attribute reading and waiting, and
/// never for write, delete, ownership, security, generic or maximum access, so a held part takes
/// part in share checks without SonicTerm being able to change it.
#[test]
pub(super) fn hold_attempts_ask_for_one_read_class_right_and_nothing_that_writes() {
    const WRITE_CLASS: u32 = 0x0002 | 0x0004 | 0x0010 | 0x0040 | 0x0100;
    const DELETE_AND_SECURITY: u32 = 0x0001_0000 | 0x0004_0000 | 0x0008_0000 | 0x0100_0000;
    const GENERIC_AND_MAXIMUM: u32 = 0xF000_0000 | 0x0200_0000;
    assert_eq!(HOLD_ACCESS_ATTEMPTS[0] & (HOLD_READ_DATA | HOLD_EXECUTE), HOLD_READ_DATA);
    assert_eq!(HOLD_ACCESS_ATTEMPTS[1] & (HOLD_READ_DATA | HOLD_EXECUTE), HOLD_EXECUTE);
    for access in HOLD_ACCESS_ATTEMPTS {
        assert_eq!(access & HOLD_BASE_ACCESS, HOLD_BASE_ACCESS, "{access:#x}");
        let forbidden = WRITE_CLASS | DELETE_AND_SECURITY | GENERIC_AND_MAXIMUM;
        assert_eq!(access & forbidden, 0, "{access:#x}");
    }
}

/// Run `open_with_read_class_access` with `answer` for every attempt, returning the outcome and
/// the access masks it requested, in order.
pub(super) fn hold_with(
    mut answer: impl FnMut(u32) -> OpenAttempt<u8>,
) -> (OpenOutcome<u8>, Vec<u32>) {
    let mut requested = Vec::new();
    let outcome = open_with_read_class_access(|access| {
        requested.push(access);
        answer(access)
    });
    (outcome, requested)
}

/// A denied read retries the same open with the next read-class right, so a folder whose listing
/// is denied is still walked. Any other result of the first attempt is final, and a part denied
/// every right is refused.
#[test]
pub(super) fn only_a_denied_read_retries_with_the_next_read_class_right() {
    let (traversed, requested) = hold_with(|access| {
        if access == HOLD_ACCESS_ATTEMPTS[0] {
            OpenAttempt::AccessDenied
        } else {
            OpenAttempt::Settled(OpenOutcome::Held(7))
        }
    });
    assert!(matches!(traversed, OpenOutcome::Held(7)));
    assert_eq!(requested, HOLD_ACCESS_ATTEMPTS);

    let (refused, requested) = hold_with(|_| OpenAttempt::AccessDenied);
    assert!(matches!(refused, OpenOutcome::Refused));
    assert_eq!(requested, HOLD_ACCESS_ATTEMPTS);

    let (read, requested) = hold_with(|_| OpenAttempt::Settled(OpenOutcome::Held(3)));
    assert!(matches!(read, OpenOutcome::Held(3)));
    assert_eq!(requested, [HOLD_ACCESS_ATTEMPTS[0]]);
    let (missing, requested) = hold_with(|_| OpenAttempt::Settled(OpenOutcome::Missing));
    assert!(matches!(missing, OpenOutcome::Missing));
    assert_eq!(requested, [HOLD_ACCESS_ATTEMPTS[0]]);
    let (blocked, requested) = hold_with(|_| OpenAttempt::Settled(OpenOutcome::Refused));
    assert!(matches!(blocked, OpenOutcome::Refused));
    assert_eq!(requested, [HOLD_ACCESS_ATTEMPTS[0]]);
}

/// The hold masks are the `windows` crate's documented rights, so the numbers the platform-neutral
/// tests check are the ones the native open passes.
#[cfg(target_os = "windows")]
#[test]
pub(super) fn hold_masks_match_the_windows_access_rights() {
    use windows::Win32::Storage::FileSystem::{
        FILE_EXECUTE, FILE_READ_ATTRIBUTES, FILE_READ_DATA, SYNCHRONIZE,
    };
    assert_eq!(HOLD_READ_DATA, FILE_READ_DATA.0);
    assert_eq!(HOLD_EXECUTE, FILE_EXECUTE.0);
    assert_eq!(HOLD_BASE_ACCESS, (FILE_READ_ATTRIBUTES | SYNCHRONIZE).0);
}

/// A fresh scratch folder for one Windows link test, under the process temporary folder.
#[cfg(target_os = "windows")]
pub(super) fn scratch_folder(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("sonicterm-links-{}-{name}", std::process::id()));
    // A folder left by an interrupted earlier run may or may not exist.
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Create a file or folder symlink. Outside CI it returns false, and the calling test is skipped,
/// when this session lacks the symlink privilege (`ERROR_PRIVILEGE_NOT_HELD`); in GitHub Actions,
/// whose runners hold it, that failure fails the test instead.
#[cfg(target_os = "windows")]
pub(super) fn create_symlink(target: &Path, link: &Path, directory: bool) -> bool {
    const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
    let created = if directory {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    match created {
        Ok(()) => true,
        Err(error)
            if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
                && std::env::var_os("GITHUB_ACTIONS").is_none() =>
        {
            eprintln!("skipped: {} needs the symlink privilege or Developer Mode", link.display());
            false
        }
        Err(error) => panic!("create symlink {}: {error}", link.display()),
    }
}

/// Create a directory junction with `mklink /J`, which needs no symlink privilege.
#[cfg(target_os = "windows")]
pub(super) fn create_junction(target: &Path, link: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J {} {}", link.display(), target.display());
}

/// The production probe with a record of every drive letter it resolves and every device and name
/// it opens, so a test can prove what a walk never reached.
#[cfg(target_os = "windows")]
#[derive(Default)]
pub(super) struct RecordingProbe {
    pub(super) calls: Vec<String>,
}

#[cfg(target_os = "windows")]
impl LocalLinkProbe for RecordingProbe {
    type Handle = std::os::windows::io::OwnedHandle;

    fn dos_device(&mut self, letter: char) -> Option<String> {
        self.calls.push(format!("dos_device {letter}"));
        NativeLinkProbe.dos_device(letter)
    }

    fn open_root(&mut self, device: &str) -> OpenOutcome<Self::Handle> {
        self.calls.push(format!("open_root {device}"));
        NativeLinkProbe.open_root(device)
    }

    fn open_child(&mut self, parent: &Self::Handle, name: &str) -> OpenOutcome<Self::Handle> {
        self.calls.push(format!("open_child {name}"));
        NativeLinkProbe.open_child(parent, name)
    }

    fn volume_device(&mut self, root: &Self::Handle) -> Option<VolumeDevice> {
        NativeLinkProbe.volume_device(root)
    }

    fn attributes(&mut self, entry: &Self::Handle) -> Option<EntryAttributes> {
        NativeLinkProbe.attributes(entry)
    }

    fn link_target(&mut self, link: &Self::Handle) -> Option<String> {
        NativeLinkProbe.link_target(link)
    }
}

/// A drive letter defined for one test and removed when dropped, even when an assertion fails.
/// It keeps the exact raw target it defined, so dropping it removes only that definition and never
/// one another program pushed onto the same letter afterwards.
#[cfg(target_os = "windows")]
pub(super) struct DefinedLetter {
    pub(super) device: Vec<u16>,
    pub(super) raw_target: Vec<u16>,
}

// Lifecycle: dropping a `DefinedLetter` removes exactly its `raw_target` definition from this logon session.
#[cfg(target_os = "windows")]
impl Drop for DefinedLetter {
    fn drop(&mut self) {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            DefineDosDeviceW, DDD_EXACT_MATCH_ON_REMOVE, DDD_RAW_TARGET_PATH, DDD_REMOVE_DEFINITION,
        };
        let removed =
            // SAFETY: `device` and `raw_target` are NUL-terminated UTF-16 strings that outlive the call.
            unsafe {
            DefineDosDeviceW(
                DDD_REMOVE_DEFINITION | DDD_EXACT_MATCH_ON_REMOVE | DDD_RAW_TARGET_PATH,
                PCWSTR(self.device.as_ptr()),
                PCWSTR(self.raw_target.as_ptr()),
            )
        };
        if let Err(error) = removed {
            eprintln!("remove drive letter definition: {error}");
        }
    }
}

/// An explicit deny ACE for Everyone on one scratch entry, removed when dropped. A deny ACE binds
/// administrators too, so an elevated CI runner still meets the restriction.
#[cfg(target_os = "windows")]
pub(super) struct DeniedRights {
    path: PathBuf,
}

#[cfg(target_os = "windows")]
impl DeniedRights {
    /// Deny `rights`, in `icacls` notation, to Everyone on `path`.
    pub(super) fn apply(path: &Path, rights: &str) -> Self {
        let status = std::process::Command::new("icacls")
            .arg(path)
            .arg("/deny")
            .arg(format!("*S-1-1-0:({rights})"))
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "icacls /deny {rights} {}", path.display());
        Self { path: path.to_path_buf() }
    }
}

// Lifecycle: dropping a `DeniedRights` removes every deny ACE for Everyone on its `path`.
#[cfg(target_os = "windows")]
impl Drop for DeniedRights {
    fn drop(&mut self) {
        let removed = std::process::Command::new("icacls")
            .arg(&self.path)
            .arg("/remove:d")
            .arg("*S-1-1-0")
            .stdout(std::process::Stdio::null())
            .status();
        if !removed.is_ok_and(|status| status.success()) {
            eprintln!("remove deny ACE on {}", self.path.display());
        }
    }
}
