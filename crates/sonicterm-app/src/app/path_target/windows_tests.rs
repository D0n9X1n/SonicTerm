//! Windows path-target tests. The link-walk tests run on every OS against an in-memory volume
//! table; the symlink and junction tests run only on Windows against real links; and ignored
//! native probes let an external driver check Explorer selection and structural-path interaction
//! in real windows.

use std::collections::BTreeMap;

use super::*;
#[cfg(target_os = "windows")]
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

// `GetDriveTypeW` results the tests report, besides `DRIVE_FIXED`.
const DRIVE_UNKNOWN: u32 = 0;
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

/// An in-memory set of Windows volumes for link-walk tests. It records every entry, link target
/// and drive type the walk reads, so a test can prove which paths were never touched. A drive
/// without a recorded type is a local fixed drive.
#[derive(Default)]
struct FakeVolumes {
    entries: BTreeMap<String, LocalEntry>,
    targets: BTreeMap<String, String>,
    drive_types: BTreeMap<char, u32>,
    entries_read: Vec<String>,
    targets_read: Vec<String>,
    drives_checked: Vec<char>,
}

impl FakeVolumes {
    fn folders(mut self, paths: &[&str]) -> Self {
        for path in paths {
            self.entries.insert((*path).to_string(), LocalEntry::Directory);
        }
        self
    }

    fn files(mut self, paths: &[&str]) -> Self {
        for path in paths {
            self.entries.insert((*path).to_string(), LocalEntry::File);
        }
        self
    }

    fn refused(mut self, path: &str) -> Self {
        self.entries.insert(path.to_string(), LocalEntry::Refused);
        self
    }

    fn link(mut self, path: &str, directory: bool, target: &str) -> Self {
        self.entries.insert(path.to_string(), LocalEntry::Link { directory });
        self.targets.insert(path.to_string(), target.to_string());
        self
    }

    fn drive(mut self, drive: char, drive_type: u32) -> Self {
        self.drive_types.insert(drive, drive_type);
        self
    }

    fn resolve(&mut self, path: &str) -> Result<PathKind, PathOpenDecision> {
        resolve_local_links(path, self)
    }

    fn touched(&self, fragment: &str) -> bool {
        self.entries_read.iter().chain(&self.targets_read).any(|path| path.contains(fragment))
    }
}

impl LocalLinkProbe for FakeVolumes {
    fn entry(&mut self, path: &str) -> LocalEntry {
        self.entries_read.push(path.to_string());
        self.entries.get(path).copied().unwrap_or(LocalEntry::Missing)
    }

    fn link_target(&mut self, path: &str) -> Option<String> {
        self.targets_read.push(path.to_string());
        self.targets.get(path).cloned()
    }

    fn drive_type(&mut self, drive: char) -> u32 {
        self.drives_checked.push(drive);
        self.drive_types.get(&drive).copied().unwrap_or(DRIVE_FIXED)
    }
}

/// Only a drive-absolute target or one relative to the link's folder names a local path; UNC,
/// device, volume, drive-relative and root-relative targets are refused from their text alone,
/// as are `/` separators, `..` after a name, and alternate-stream or reserved names.
#[test]
fn link_targets_parse_only_local_drive_and_relative_forms() {
    fn absolute(drive: char, names: &[&str]) -> Option<LinkTarget> {
        let names = names.iter().map(|name| (*name).to_string()).collect();
        Some(LinkTarget::Absolute { drive, names })
    }
    fn relative(parent_steps: usize, names: &[&str]) -> Option<LinkTarget> {
        let names = names.iter().map(|name| (*name).to_string()).collect();
        Some(LinkTarget::Relative { parent_steps, names })
    }
    let cases = [
        (r"C:\real\notes.txt", absolute('C', &["real", "notes.txt"])),
        (r"c:\real\", absolute('C', &["real"])),
        (r"\\?\C:\real\notes.txt", absolute('C', &["real", "notes.txt"])),
        (r"\??\D:\data", absolute('D', &["data"])),
        (r"\??\C:\", absolute('C', &[])),
        ("notes.txt", relative(0, &["notes.txt"])),
        (r".\sub\notes.txt", relative(0, &["sub", "notes.txt"])),
        (r"..\..\other\", relative(2, &["other"])),
        (r"\\host\share\notes.txt", None),
        (r"\\?\UNC\host\share\notes.txt", None),
        (r"\??\UNC\host\share\notes.txt", None),
        (r"\\.\pipe\host", None),
        (r"\\?\Volume{00000000-0000-0000-0000-000000000000}\data", None),
        (r"\??\GLOBALROOT\Device\Mup\host\share", None),
        (r"\??\C:", None),
        ("C:notes.txt", None),
        (r"\real\notes.txt", None),
        ("C:/real/notes.txt", None),
        ("sub/notes.txt", None),
        (r"sub\..\notes.txt", None),
        (r"C:\real\..\notes.txt", None),
        (r"C:\real\.\notes.txt", None),
        (r"C:\\notes.txt", None),
        ("notes.txt:stream", None),
        (r"C:\real\notes.txt:stream", None),
        ("notes.", None),
        ("notes ", None),
        ("no*tes", None),
        ("sub\\\u{1}name", None),
        ("", None),
    ];
    for (raw, expected) in cases {
        assert_eq!(parse_link_target(raw), expected, "{raw}");
    }
}

/// Only a local fixed drive may hold a followed link or receive its target; a mapped network
/// share, removable or optical media, a RAM disk and an unknown root are refused.
#[test]
fn only_local_fixed_drives_carry_followed_links() {
    assert!(drive_type_is_local_fixed(DRIVE_FIXED));
    for drive_type in [
        DRIVE_UNKNOWN,
        DRIVE_NO_ROOT_DIR,
        DRIVE_REMOVABLE,
        DRIVE_REMOTE,
        DRIVE_CDROM,
        DRIVE_RAMDISK,
    ] {
        assert!(!drive_type_is_local_fixed(drive_type), "drive type {drive_type}");
    }
}

/// A path without links walks from its drive root one entry at a time and never reads a link
/// target or a drive type, so a plain path resolves on every drive, a mapped network drive too.
#[test]
fn plain_paths_walk_from_the_root_without_link_or_drive_reads() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"Z:\", r"Z:\share"])
        .files(&[r"C:\work\notes.txt", r"Z:\share\notes.txt"])
        .drive('Z', DRIVE_REMOTE);
    assert_eq!(volumes.resolve(r"C:\work\notes.txt"), Ok(PathKind::File));
    assert_eq!(volumes.entries_read, [r"C:\", r"C:\work", r"C:\work\notes.txt"]);
    // `.` and `..` resolve lexically before any lookup, as Win32 path normalization does.
    assert_eq!(volumes.resolve("c:/work/./sub/../notes.txt"), Ok(PathKind::File));
    assert_eq!(volumes.resolve(r"C:\work"), Ok(PathKind::Directory));
    assert_eq!(volumes.resolve(r"C:\"), Ok(PathKind::Directory));
    assert_eq!(volumes.resolve(r"Z:\share\notes.txt"), Ok(PathKind::File));
    assert_eq!(volumes.resolve(r"C:\work\gone.txt"), Err(PathOpenDecision::Missing));
    assert_eq!(volumes.resolve(r"C:\gone\notes.txt"), Err(PathOpenDecision::Missing));
    // Windows reports a path through a file as not found, so it stays missing.
    assert_eq!(volumes.resolve(r"C:\work\notes.txt\more"), Err(PathOpenDecision::Missing));
    assert_eq!(volumes.resolve(r"Y:\notes.txt"), Err(PathOpenDecision::Missing));
    assert!(volumes.targets_read.is_empty());
    assert!(volumes.drives_checked.is_empty());
}

/// A file symlink on a local fixed drive is followed whether `read_link` reports its target as a
/// drive path, an NT `\??\` path, or a path relative to the link's folder.
#[test]
fn file_links_on_local_fixed_drives_resolve_to_their_targets() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\real"])
        .files(&[r"C:\real\notes.txt"])
        .link(r"C:\work\absolute.txt", false, r"C:\real\notes.txt")
        .link(r"C:\work\nt.txt", false, r"\??\C:\real\notes.txt")
        .link(r"C:\work\relative.txt", false, r"..\real\notes.txt");
    for link in [r"C:\work\absolute.txt", r"C:\work\nt.txt", r"C:\work\relative.txt"] {
        assert_eq!(volumes.resolve(link), Ok(PathKind::File), "{link}");
    }
    assert_eq!(volumes.targets_read.len(), 3);
    assert!(volumes.drives_checked.iter().all(|drive| *drive == 'C'));
}

/// A junction partway along a path is replaced by its validated target before the walk goes
/// deeper, so every later read goes through ordinary folders, never through the junction.
#[test]
fn junction_ancestors_are_replaced_by_their_targets_before_descending() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\real", r"C:\real\sub"])
        .files(&[r"C:\real\sub\notes.txt"])
        .link(r"C:\work\junction", true, r"C:\real");
    assert_eq!(volumes.resolve(r"C:\work\junction\sub\notes.txt"), Ok(PathKind::File));
    assert_eq!(
        volumes.entries_read,
        [
            r"C:\",
            r"C:\work",
            r"C:\work\junction",
            r"C:\",
            r"C:\real",
            r"C:\real\sub",
            r"C:\real\sub\notes.txt",
        ]
    );
    assert_eq!(volumes.targets_read, [r"C:\work\junction"]);
    assert_eq!(volumes.resolve(r"C:\work\junction"), Ok(PathKind::Directory));
}

/// A link naming a UNC share or a device path is refused from its text, so the walk never reads
/// anything on the host it names, whether the link is the final name or a folder along the path.
#[test]
fn links_to_unc_shares_and_devices_are_refused_without_touching_the_host() {
    for target in [
        r"\\host\share\notes.txt",
        r"\\?\UNC\host\share\notes.txt",
        r"\??\UNC\host\share\notes.txt",
        r"\\.\pipe\host",
        r"\??\GLOBALROOT\Device\Mup\host\share",
    ] {
        let mut volumes = FakeVolumes::default()
            .folders(&[r"C:\", r"C:\work"])
            .link(r"C:\work\file-link.txt", false, target)
            .link(r"C:\work\folder-link", true, target);
        let blocked = Err(PathOpenDecision::Blocked);
        assert_eq!(volumes.resolve(r"C:\work\file-link.txt"), blocked, "{target}");
        assert_eq!(volumes.resolve(r"C:\work\folder-link\notes.txt"), blocked, "{target}");
        assert!(!volumes.touched("host"), "{target}");
    }
}

/// A link whose target is on a drive letter mapped to a network share is refused before the walk
/// reads anything on that drive; the same link resolves when that letter is a local fixed drive.
#[test]
fn links_through_mapped_network_drives_are_refused_before_any_read() {
    let volumes_with = |drive_type: u32| {
        FakeVolumes::default()
            .folders(&[r"C:\", r"C:\work", r"Z:\", r"Z:\share"])
            .files(&[r"Z:\share\notes.txt"])
            .link(r"C:\work\mapped.txt", false, r"Z:\share\notes.txt")
            .link(r"C:\work\mapped-folder", true, r"\??\Z:\share")
            .drive('Z', drive_type)
    };
    let mut remote = volumes_with(DRIVE_REMOTE);
    assert_eq!(remote.resolve(r"C:\work\mapped.txt"), Err(PathOpenDecision::Blocked));
    assert_eq!(remote.resolve(r"C:\work\mapped-folder\notes.txt"), Err(PathOpenDecision::Blocked));
    assert!(!remote.touched(r"Z:\"));
    let mut fixed = volumes_with(DRIVE_FIXED);
    assert_eq!(fixed.resolve(r"C:\work\mapped.txt"), Ok(PathKind::File));
    assert_eq!(fixed.resolve(r"C:\work\mapped-folder\notes.txt"), Ok(PathKind::File));
}

/// A link held on a mapped network share or a removable drive is never followed, even toward a
/// local fixed drive, and its target is never read; a plain file beside it still resolves.
#[test]
fn links_held_on_non_fixed_drives_are_refused_before_their_targets_are_read() {
    for drive_type in [DRIVE_REMOTE, DRIVE_REMOVABLE] {
        let mut volumes = FakeVolumes::default()
            .folders(&[r"C:\", r"C:\real", r"Z:\", r"Z:\share"])
            .files(&[r"C:\real\notes.txt", r"Z:\share\notes.txt"])
            .link(r"Z:\share\link.txt", false, r"C:\real\notes.txt")
            .drive('Z', drive_type);
        let blocked = Err(PathOpenDecision::Blocked);
        assert_eq!(volumes.resolve(r"Z:\share\link.txt"), blocked, "{drive_type}");
        assert!(volumes.targets_read.is_empty(), "{drive_type}");
        assert_eq!(volumes.resolve(r"Z:\share\notes.txt"), Ok(PathKind::File), "{drive_type}");
    }
}

/// A reparse point other than a symlink or junction, such as a cloud-file placeholder, is refused
/// both as the final name and as a folder along the path.
#[test]
fn other_reparse_points_are_refused() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work"])
        .refused(r"C:\work\placeholder.txt")
        .refused(r"C:\work\cloud");
    assert_eq!(volumes.resolve(r"C:\work\placeholder.txt"), Err(PathOpenDecision::Blocked));
    assert_eq!(volumes.resolve(r"C:\work\cloud\notes.txt"), Err(PathOpenDecision::Blocked));
    assert!(volumes.targets_read.is_empty());
}

/// A dangling final link is refused, as on macOS and Linux, while a missing name below a link
/// stays missing, as it does below a folder, so a shorter candidate path can still be tried.
#[test]
fn dangling_final_links_block_while_missing_names_below_links_stay_missing() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\real"])
        .link(r"C:\work\dangling.txt", false, r"C:\real\gone.txt")
        .link(r"C:\work\gone-folder", true, r"C:\gone")
        .link(r"C:\work\real-folder", true, r"C:\real");
    assert_eq!(volumes.resolve(r"C:\work\dangling.txt"), Err(PathOpenDecision::Blocked));
    assert_eq!(volumes.resolve(r"C:\work\gone-folder"), Err(PathOpenDecision::Blocked));
    let missing = Err(PathOpenDecision::Missing);
    assert_eq!(volumes.resolve(r"C:\work\gone-folder\notes.txt"), missing);
    assert_eq!(volumes.resolve(r"C:\work\real-folder\gone.txt"), missing);
}

/// A link to itself, a loop between two folder links, and a chain one hop longer than
/// `MAX_LINK_HOPS` are refused; a chain of exactly `MAX_LINK_HOPS` links still resolves.
#[test]
fn link_loops_and_overlong_chains_are_refused() {
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
fn links_must_reach_the_kind_their_own_folder_flag_promises() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\real"])
        .files(&[r"C:\real\notes.txt"])
        .link(r"C:\work\folder-to-file", true, r"C:\real\notes.txt")
        .link(r"C:\work\file-to-folder.txt", false, r"C:\real");
    let blocked = Err(PathOpenDecision::Blocked);
    assert_eq!(volumes.resolve(r"C:\work\folder-to-file"), blocked);
    assert_eq!(volumes.resolve(r"C:\work\file-to-folder.txt"), blocked);
    assert_eq!(volumes.resolve(r"C:\work\file-to-folder.txt\notes.txt"), blocked);
}

/// A relative target climbs from the link's folder with leading `..` names, but a target that
/// would climb above the drive root is refused rather than clamped to it.
#[test]
fn relative_targets_climb_from_the_link_folder_but_not_above_the_root() {
    let mut volumes = FakeVolumes::default()
        .folders(&[r"C:\", r"C:\work", r"C:\work\sub"])
        .files(&[r"C:\notes.txt"])
        .link(r"C:\work\sub\up.txt", false, r"..\..\notes.txt")
        .link(r"C:\work\sub\over.txt", false, r"..\..\..\notes.txt");
    assert_eq!(volumes.resolve(r"C:\work\sub\up.txt"), Ok(PathKind::File));
    assert_eq!(volumes.resolve(r"C:\work\sub\over.txt"), Err(PathOpenDecision::Blocked));
}

/// A chain of links whose targets are long paths is refused once the walk has read
/// `MAX_WALK_ENTRIES` entries, well before `MAX_LINK_HOPS`; the same chain resolves when shorter.
#[test]
fn long_link_chains_stop_at_the_entry_read_bound() {
    // Each hop names an absolute target 100 folders deep, so it costs 102 entry reads.
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
                end.clone()
            } else {
                format!(r"{deep}\{}.txt", link_index + 1)
            };
            volumes = volumes.link(&format!(r"{deep}\{link_index}.txt"), false, &target);
        }
        volumes
    };
    let first_link = format!(r"{deep}\0.txt");
    let mut long = chain(12);
    assert_eq!(long.resolve(&first_link), Err(PathOpenDecision::Blocked));
    assert_eq!(long.entries_read.len(), MAX_WALK_ENTRIES);
    assert!(long.targets_read.len() < MAX_LINK_HOPS);
    assert_eq!(chain(4).resolve(&first_link), Ok(PathKind::File));
}

/// Only a drive-absolute path is walked: UNC, verbatim, device, rooted, drive-relative and
/// relative paths are refused before any read.
#[test]
fn only_drive_absolute_paths_are_walked() {
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
    assert!(volumes.entries_read.is_empty());
}

/// A fresh scratch folder for one Windows link test, under the process temporary folder.
#[cfg(target_os = "windows")]
fn scratch_folder(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("sonicterm-links-{}-{name}", std::process::id()));
    // A folder left by an interrupted earlier run may or may not exist.
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Create a file or folder symlink. It returns false, and the calling test is skipped, only when
/// this Windows session lacks the symlink privilege (`ERROR_PRIVILEGE_NOT_HELD`).
#[cfg(target_os = "windows")]
fn create_symlink(target: &Path, link: &Path, directory: bool) -> bool {
    const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
    let created = if directory {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    match created {
        Ok(()) => true,
        Err(error) if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => {
            eprintln!("skipped: {} needs the symlink privilege or Developer Mode", link.display());
            false
        }
        Err(error) => panic!("create symlink {}: {error}", link.display()),
    }
}

/// Create a directory junction with `mklink /J`, which needs no symlink privilege.
#[cfg(target_os = "windows")]
fn create_junction(target: &Path, link: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J {} {}", link.display(), target.display());
}

/// The production probe with a record of every entry and link it reads, reporting the drive
/// letters in `remote_drives` as mapped network drives, so a test can prove what a walk never read.
#[cfg(target_os = "windows")]
#[derive(Default)]
struct RecordingProbe {
    remote_drives: Vec<char>,
    reads: Vec<String>,
}

#[cfg(target_os = "windows")]
impl LocalLinkProbe for RecordingProbe {
    fn entry(&mut self, path: &str) -> LocalEntry {
        self.reads.push(path.to_string());
        NativeLinkProbe.entry(path)
    }

    fn link_target(&mut self, path: &str) -> Option<String> {
        self.reads.push(path.to_string());
        NativeLinkProbe.link_target(path)
    }

    fn drive_type(&mut self, drive: char) -> u32 {
        if self.remote_drives.contains(&drive) {
            DRIVE_REMOTE
        } else {
            NativeLinkProbe.drive_type(drive)
        }
    }
}

/// A file symlink on a local fixed drive, with an absolute or a relative target, is selected in
/// its folder like the file it names, and reveal-time revalidation accepts the same chain.
#[cfg(target_os = "windows")]
#[test]
fn native_symlinked_files_are_selected_in_their_folder() {
    let root = scratch_folder("file-symlink");
    std::fs::create_dir_all(root.join("real")).unwrap();
    std::fs::write(root.join(r"real\notes.txt"), "notes").unwrap();
    let absolute = root.join("absolute.txt");
    let relative = root.join("relative.txt");
    let created = create_symlink(&root.join(r"real\notes.txt"), &absolute, false)
        && create_symlink(Path::new(r"real\notes.txt"), &relative, false);
    if !created {
        std::fs::remove_dir_all(&root).unwrap();
        return;
    }
    for link in [&absolute, &relative] {
        let decision = classify_local_target(link);
        assert_eq!(decision, PathOpenDecision::Openable(PathKind::File), "{}", link.display());
        assert_eq!(local_target_action(decision), Some(LocalTargetAction::Reveal));
        validate_reveal_target(link, decision).unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// A junction to a local folder navigates like that folder: it classifies as a directory,
/// dispatch-time revalidation accepts it, and a file reached through it is selected.
#[cfg(target_os = "windows")]
#[test]
fn native_junctioned_folders_are_opened() {
    let root = scratch_folder("junction");
    std::fs::create_dir_all(root.join("real")).unwrap();
    std::fs::write(root.join(r"real\notes.txt"), "notes").unwrap();
    let junction = root.join("junction");
    create_junction(&root.join("real"), &junction);
    let decision = classify_local_target(&junction);
    assert_eq!(decision, PathOpenDecision::Openable(PathKind::Directory));
    assert_eq!(local_target_action(decision), Some(LocalTargetAction::Navigate));
    validate_windows_directory(&junction, decision).unwrap();
    let through = junction.join("notes.txt");
    let file_decision = classify_local_target(&through);
    assert_eq!(file_decision, PathOpenDecision::Openable(PathKind::File));
    validate_reveal_target(&through, file_decision).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// A symlink to a `\\host\share` path is refused from its target text: the walk reads only local
/// entries, never anything on the host, whether the link is a file, a folder, or a folder on the
/// path. The `.invalid` host name can never resolve.
#[cfg(target_os = "windows")]
#[test]
fn native_unc_symlinks_are_refused_without_contacting_the_host() {
    let root = scratch_folder("unc-link");
    let share = Path::new(r"\\sonicterm-unreachable.invalid\share");
    let file_link = root.join("share.txt");
    let folder_link = root.join("share-folder");
    let created = create_symlink(&share.join("notes.txt"), &file_link, false)
        && create_symlink(share, &folder_link, true);
    if !created {
        std::fs::remove_dir_all(&root).unwrap();
        return;
    }
    let mut probe = RecordingProbe::default();
    for path in [file_link.clone(), folder_link.clone(), folder_link.join("notes.txt")] {
        let decision = classify_windows_target_with(&path, &mut probe);
        assert_eq!(decision, PathOpenDecision::Blocked, "{}", path.display());
        assert_eq!(classify_local_target(&path), PathOpenDecision::Blocked, "{}", path.display());
    }
    assert!(probe
        .reads
        .iter()
        .all(|read| !read.starts_with(r"\\") && !read.contains("unreachable")));
    std::fs::remove_dir_all(root).unwrap();
}

/// A file or folder symlink to a drive letter mapped to a network share is refused before
/// anything on that drive is read. The probe reports an unused letter as `DRIVE_REMOTE`, so the
/// test needs no real share.
#[cfg(target_os = "windows")]
#[test]
fn native_mapped_drive_symlinks_are_refused_before_any_read() {
    let remote_drive = ('D'..='Z')
        .rev()
        .find(|drive| NativeLinkProbe.drive_type(*drive) == DRIVE_NO_ROOT_DIR)
        .expect("an unused drive letter");
    let remote_root = format!(r"{remote_drive}:\");
    let root = scratch_folder("mapped-drive");
    let file_link = root.join("mapped.txt");
    let folder_link = root.join("mapped-folder");
    let created =
        create_symlink(&Path::new(&remote_root).join(r"share\notes.txt"), &file_link, false)
            && create_symlink(&Path::new(&remote_root).join("share"), &folder_link, true);
    if !created {
        std::fs::remove_dir_all(&root).unwrap();
        return;
    }
    let mut probe = RecordingProbe { remote_drives: vec![remote_drive], ..Default::default() };
    for path in [file_link.clone(), folder_link.clone(), folder_link.join("notes.txt")] {
        let decision = classify_windows_target_with(&path, &mut probe);
        assert_eq!(decision, PathOpenDecision::Blocked, "{}", path.display());
    }
    assert!(probe.reads.iter().all(|read| !read.starts_with(&remote_root)));
    std::fs::remove_dir_all(root).unwrap();
}

/// Dispatch revalidates the whole chain: a file link and a folder link that named local targets
/// when probed, then were retargeted to a network share, are refused by the reveal and navigation
/// checks that run before any native call.
#[cfg(target_os = "windows")]
#[test]
fn native_dispatch_refuses_links_retargeted_to_a_network_share() {
    let root = scratch_folder("retarget");
    std::fs::create_dir_all(root.join("real")).unwrap();
    std::fs::write(root.join(r"real\notes.txt"), "notes").unwrap();
    let file_link = root.join("notes-link.txt");
    let folder_link = root.join("folder-link");
    let created = create_symlink(&root.join(r"real\notes.txt"), &file_link, false)
        && create_symlink(&root.join("real"), &folder_link, true);
    if !created {
        std::fs::remove_dir_all(&root).unwrap();
        return;
    }
    let file_decision = classify_local_target(&file_link);
    let folder_decision = classify_local_target(&folder_link);
    assert_eq!(file_decision, PathOpenDecision::Openable(PathKind::File));
    assert_eq!(folder_decision, PathOpenDecision::Openable(PathKind::Directory));
    // Removing a symlink removes only the link, never its target.
    std::fs::remove_file(&file_link).unwrap();
    std::fs::remove_dir(&folder_link).unwrap();
    let share = Path::new(r"\\sonicterm-unreachable.invalid\share");
    assert!(create_symlink(&share.join("notes.txt"), &file_link, false));
    assert!(create_symlink(share, &folder_link, true));
    let reveal = validate_reveal_target(&file_link, file_decision).unwrap_err();
    assert_eq!(reveal.kind(), io::ErrorKind::PermissionDenied);
    let navigate = validate_windows_directory(&folder_link, folder_decision).unwrap_err();
    assert_eq!(navigate.kind(), io::ErrorKind::PermissionDenied);
    std::fs::remove_dir_all(root).unwrap();
}

/// Native manual probe uses production validation/dispatch; Explorer selection is inspected by the caller.
#[cfg(target_os = "windows")]
#[test]
#[ignore = "opens Explorer for an explicitly supplied native test fixture"]
fn reveal_file_in_explorer_native_probe() {
    let path =
        PathBuf::from(std::env::var_os("SONICTERM_REVEAL_PROBE_FILE").expect("explicit fixture"));
    let decision = classify_local_target(&path);
    assert_eq!(decision, PathOpenDecision::Openable(PathKind::File));
    open_path(&path, decision).expect("native Explorer selection request");
}

/// A dedicated process runs real native input and path workers with scratch state; the driver verifies Explorer selection.
#[cfg(target_os = "windows")]
#[test]
#[ignore = "requires an external native pointer driver and explicit scratch directory"]
fn structural_paths_native_interaction() {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
    };
    struct NativeProbe {
        app: App,
        root: PathBuf,
        started: std::time::Instant,
        sequence: u64,
        input_sequence: u64,
        last_pointer_event: serde_json::Value,
    }
    impl ApplicationHandler<super::super::super::UserEvent> for NativeProbe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.app.resumed(event_loop);
        }
        fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
            self.app.new_events(event_loop, cause);
        }
        fn user_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            event: super::super::super::UserEvent,
        ) {
            self.app.user_event(event_loop, event);
        }
        fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
            if matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::CursorEntered { .. }
                    | WindowEvent::CursorLeft { .. }
                    | WindowEvent::ModifiersChanged(_)
                    | WindowEvent::Focused(_)
                    | WindowEvent::MouseInput { .. }
            ) {
                self.input_sequence += 1;
                self.last_pointer_event = serde_json::json!({
                    "window": format!("{id:?}"),
                    "event": format!("{event:?}"),
                    "sequence": self.input_sequence,
                });
            }
            self.app.window_event(event_loop, id, event);
        }
        fn device_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            id: winit::event::DeviceId,
            event: winit::event::DeviceEvent,
        ) {
            self.app.device_event(event_loop, id, event);
        }
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            self.app.about_to_wait(event_loop);
            let native_windows = self.app.windows.iter().filter_map(|(id, state)| {
                let window = state.window.as_ref()?;
                let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else { return None };
                let active_pane = state.tab_states.get(state.tabs.active_index())?.active_pane;
                let pane = state.panes.get(&active_pane)?;
                let parser = pane.parser.try_lock()?;
                let rows = parser.grid().rows_iter().map(|row| row.iter().map(|cell| cell.ch).collect::<String>()).collect::<Vec<_>>();
                Some(serde_json::json!({"hwnd":handle.hwnd.get(),"main":Some(*id)==self.app.main_window_id,"width":window.inner_size().width,"height":window.inner_size().height,"scale":window.scale_factor(),"tabs":state.tabs.len(),"rows":rows,"hidden":state.hidden}))
            }).collect::<Vec<_>>();
            std::fs::write(
                self.root.join("windows.json"),
                serde_json::to_vec(&native_windows).unwrap(),
            )
            .unwrap();
            if let Some(id) = self.app.main_window_id {
                let window = &self.app.windows[&id];
                let tab = &window.tab_states[window.tabs.active_index()];
                if let (Some(native), Some(renderer), Some(pane)) =
                    (&window.window, &window.renderer, window.panes.get(&tab.active_pane))
                {
                    if let Some(parser) = pane.parser.try_lock() {
                        let RawWindowHandle::Win32(handle) =
                            native.window_handle().unwrap().as_raw()
                        else {
                            panic!("Windows handle")
                        };
                        let rows = parser
                            .grid()
                            .rows_iter()
                            .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
                            .collect::<Vec<_>>();
                        let (cell_width, cell_height) = renderer.cell_size();
                        let pane_id = window.tab_states[window.tabs.active_index()].active_pane;
                        let origin = renderer.pane_grid_origin(pane_id);
                        let pointer_cell = renderer.pixel_to_pane_cell(
                            window.cursor_pos.0 as f32,
                            window.cursor_pos.1 as f32,
                        );
                        // Observe the held parser without advancing probes or granting fresh authorization.
                        let fresh = pointer_cell
                            .filter(|(pointed_pane, _, _)| *pointed_pane == pane_id)
                            .and_then(|(_, row, col)| {
                                self.app.cell_target_from_parser(
                                    id,
                                    pane_id,
                                    row,
                                    col,
                                    &parser,
                                    pane.viewport_top_abs,
                                )
                            });
                        let probe = &window.path_probe;
                        let fresh_key = fresh.as_ref().and_then(|target| match &target.target {
                            ResolvedCellTarget::Path(key) => Some(key),
                            _ => None,
                        });
                        let current_matches =
                            fresh_key.is_some_and(|key| probe.current.as_ref() == Some(key));
                        let settled = if fresh_key.is_some() {
                            current_matches
                                && probe.pending_result.is_none()
                                && (probe.selection.is_some() || probe.failure.is_some())
                        } else {
                            fresh.is_none()
                                && pointer_cell.is_some_and(|(pane, _, _)| pane == pane_id)
                                && probe.current.is_none()
                                && probe.pending_result.is_none()
                        };
                        let mut native_cursor = windows::Win32::Foundation::POINT::default();
                        let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut _);
                        let (native_cursor_screen, native_cursor_client, foreground) =
                            // SAFETY: native keeps hwnd live; native_cursor is writable and other handles are only compared.
                            unsafe {
                                use windows::Win32::{
                                    Graphics::Gdi::ScreenToClient,
                                    UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow},
                                };
                                let screen = GetCursorPos(&mut native_cursor)
                                    .ok()
                                    .map(|()| [native_cursor.x, native_cursor.y]);
                                let client = screen.and_then(|_| {
                                    ScreenToClient(hwnd, &mut native_cursor)
                                        .as_bool()
                                        .then_some([native_cursor.x, native_cursor.y])
                                });
                                (screen, client, GetForegroundWindow() == hwnd)
                            };
                        self.sequence += 1;
                        let report = serde_json::json!({
                            "sequence": self.sequence,
                            "input_sequence": self.input_sequence,
                            "last_pointer_event": self.last_pointer_event,
                            "cursor_pos": [window.cursor_pos.0, window.cursor_pos.1],
                            "native_cursor_screen": native_cursor_screen,
                            "native_cursor_client": native_cursor_client,
                            "foreground": foreground,
                            "open_modifier": self.app.open_modifier_held(id),
                            "probe": {
                                "epoch": probe.epoch.0,
                                "pointed": probe.current.as_ref().map(|key| [key.pointed.row, u64::from(key.pointed.col)]),
                                "current_matches": current_matches,
                                "settled": settled,
                                "pending_result": probe.pending_result.is_some(),
                                "failure": probe.failure,
                                "selection": probe.selection.as_ref().map(|selection| selection.candidate.resolved_path.to_string_lossy()),
                                "fresh_kind": match fresh.as_ref().map(|target| &target.target) {
                                    None => "none",
                                    Some(ResolvedCellTarget::Path(_)) => "path",
                                    Some(ResolvedCellTarget::Uri(_)) => "uri",
                                    Some(ResolvedCellTarget::Rejected(_)) => "rejected",
                                },
                            },
                            "hwnd": handle.hwnd.get(), "rows": rows, "cw": cell_width, "ch": cell_height,
                            "top": origin.map(|grid_origin| grid_origin[1]), "tab_bar_top": renderer.tab_bar_y_offset(),
                            "surface_height": renderer.height(), "padding_bottom": renderer.padding_bottom_px(),
                            "view_top": GpuRenderer::resolved_view_top_abs_legacy(parser.grid(), pane.viewport_top_abs),
                            "search_current": tab.search.as_ref().and_then(|search| search.current),
                            "search_total": tab.search.as_ref().map(|search| search.matches.len()),
                            "pointer_cell": renderer.pixel_to_pane_cell(window.cursor_pos.0 as f32, window.cursor_pos.1 as f32),
                            "selection_rows": window.selection.as_ref().map(|selection| {let (start,end)=selection.normalized(); [start.0,end.0]}),
                            "padding_left": self.app.config.window.padding_left,
                            "preview": window.link_preview.as_ref().map(|preview| &preview.uri),
                            "notification": window.notification.as_ref().map(|notification| &notification.message),
                            "links": parser.grid().rows_iter().enumerate().flat_map(|(row, cells)| cells.iter().enumerate().filter_map(move |(col, cell)| cell.hyperlink().map(|id| (row,col,id)))).filter_map(|(row,col,id)| parser.hyperlinks().lookup(id).map(|link| serde_json::json!({"row":row,"col":col,"uri":link.uri}))).collect::<Vec<_>>()});
                        std::fs::write(
                            self.root.join("window.json"),
                            serde_json::to_vec(&report).unwrap(),
                        )
                        .unwrap();
                    }
                }
            }
            if self.root.join("done").exists() {
                event_loop.exit();
            }
            assert!(
                self.started.elapsed() < std::time::Duration::from_secs(180),
                "native driver deadline"
            );
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(50),
            ));
        }
    }
    assert!(std::env::var_os("NO_COLOR").is_none(), "native color profile required");
    let root = PathBuf::from(
        std::env::var_os("SONICTERM_PATH_INTERACTION_DIR").expect("explicit scratch directory"),
    );
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::create_dir_all(root.join("logs")).unwrap();
    let mut config = Config::default();
    config.terminal.shell = Some("cmd.exe".into());
    if root.join("cold").exists() {
        config.window.warm_window_pool = 0;
    }
    config.window.cols = 110;
    config.window.rows = 28;
    config.logging.level = sonicterm_logging::LogLevel::Debug;
    let _log = sonicterm_logging::init_in(&config.logging, &root.join("logs")).unwrap();
    tracing::warn!(
        target: "sonicterm_app::app::path_target::path_target_tests",
        "native path interaction started"
    );
    let event_loop = EventLoop::<super::super::super::UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .unwrap();
    let mut app = App::new_with_proxy(
        Theme::default(),
        config,
        Keymap::parse_resilient(
            &format!(
                "{}\n[[binding]]\nkeys = \"alt+shift+x\"\naction = \"move_tab_to_new_window\"\n",
                include_str!("../../../../../assets/keymaps/sonicterm-windows.toml")
            ),
            "native fixture",
        )
        .unwrap(),
        Some(event_loop.create_proxy()),
    );
    // Inject only fixture path resolution; the child shell must keep the user's real HOME.
    if root.join("home").is_dir() {
        app.home_dir = Some(root.join("home"));
    }
    app.runtime_config_path = Some(root.join("config/sonicterm.toml"));
    let mut probe = NativeProbe {
        app,
        root,
        started: std::time::Instant::now(),
        sequence: 0,
        input_sequence: 0,
        last_pointer_event: serde_json::Value::Null,
    };
    event_loop.run_app(&mut probe).unwrap();
    assert!(
        probe.root.join("done").exists(),
        "driver must verify native outcomes before completion"
    );
}
