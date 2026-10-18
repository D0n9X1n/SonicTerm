//! Decide what a macOS document open may do with a file: draft it, run it, or refuse it.
//!
//! A document open never executes a script. Only a positively identified native
//! Mach-O executable that is a regular file, carries no quarantine attribute and
//! passes an execute-permission check may run. Every check after canonicalization
//! reads the same opened file descriptor, and the file's device and inode are
//! recorded so the app can confirm the path still names that file just before
//! spawn. Every classification error fails closed.

use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use sonicterm_types::{FileIdentity, OpenScriptRequest};

/// File extensions that always mark a script, whatever the file's content.
const SCRIPT_EXTENSIONS: &[&str] =
    &["sh", "command", "tool", "bash", "zsh", "csh", "tcsh", "ksh", "fish"];
/// The extended attribute macOS sets on downloaded files.
const QUARANTINE_ATTRIBUTE: &str = "com.apple.quarantine";
/// The largest architecture count accepted in a fat header; Java class files share its magic.
const MAX_FAT_ARCHITECTURES: u32 = 16;
/// Thin Mach-O magic numbers, read big-endian from the first four bytes.
const THIN_MACH_O_MAGICS: [u32; 4] = [0xfeed_face, 0xcefa_edfe, 0xfeed_facf, 0xcffa_edfe];
/// Fat (universal) Mach-O magic numbers, always stored big-endian.
const FAT_MACH_O_MAGICS: [u32; 2] = [0xcafe_babe, 0xcafe_babf];

/// Why a document open was refused instead of drafted or run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Refusal {
    /// The path could not be canonicalized, for example a broken link.
    Unresolvable,
    /// The path is, or lies inside, an application bundle.
    Bundle,
    /// The path names a directory.
    Directory,
    /// The path names something other than a regular file or directory.
    NotRegular,
    /// The file could not be opened, read or inspected.
    Unreadable,
    /// The file carries `com.apple.quarantine`, or its attributes could not be read.
    Quarantined,
    /// The file is a Mach-O executable that fails the execute-permission check.
    NotExecutable,
}

impl Refusal {
    /// The stable reason logged at `warn`; it never names the file.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unresolvable => "unresolvable",
            Self::Bundle => "bundle",
            Self::Directory => "directory",
            Self::NotRegular => "not_regular",
            Self::Unreadable => "unreadable",
            Self::Quarantined => "quarantined",
            Self::NotExecutable => "not_executable",
        }
    }

    /// The user-facing notice, naming only the file's printable name.
    pub(crate) fn notice(self, file_name: &str) -> String {
        match self {
            Self::Unresolvable => format!("Not run: {file_name} could not be resolved."),
            Self::Bundle => {
                format!("Not run: {file_name} is an application bundle or lies inside one.")
            }
            Self::Directory => format!("Not run: {file_name} is a folder."),
            Self::NotRegular => format!("Not run: {file_name} is not a regular file."),
            Self::Unreadable => format!("Not run: {file_name} could not be read."),
            Self::Quarantined => format!(
                "Not run: {file_name} is quarantined (downloaded). Run it yourself after review."
            ),
            Self::NotExecutable => format!("Not run: {file_name} is not executable."),
        }
    }
}

/// What a document open does with one file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Disposition {
    /// Today's script path: a draft for a supported script, else a warning and no draft.
    Draft(OpenScriptRequest),
    /// Run this verified native executable directly.
    Run {
        /// The canonical absolute path that runs.
        program: PathBuf,
        /// The canonical parent directory, used as the working directory.
        cwd: PathBuf,
        /// The device and inode read from the classified descriptor.
        identity: FileIdentity,
    },
    /// Open a plain shell tab in `cwd` (or home when `None`) and show the refusal.
    Refuse {
        /// Why nothing ran.
        refusal: Refusal,
        /// The shell tab's directory; `None` falls back to home.
        cwd: Option<PathBuf>,
        /// The file's printable name for the notice.
        file_name: String,
    },
}

/// Classify one resolved document-open request.
pub(crate) fn classify(request: OpenScriptRequest) -> Disposition {
    let file_name = sonicterm_types::printable_file_name(request.launch_path.as_os_str());
    let refuse = |refusal: Refusal, cwd: Option<PathBuf>| Disposition::Refuse {
        refusal,
        cwd,
        file_name: file_name.clone(),
    };
    let Ok(canonical) = std::fs::canonicalize(&request.launch_path) else {
        // When: `canonicalize` fails (a broken link or a missing component), fail closed in home.
        return refuse(Refusal::Unresolvable, None);
    };
    let parent = canonical.parent().map(Path::to_path_buf);
    if let Some(outer_bundle) = outermost_bundle(&canonical) {
        // When: `outer_bundle` exists, the target is a bundle or inside one; open beside it, never inside.
        return refuse(Refusal::Bundle, outer_bundle.parent().map(Path::to_path_buf));
    }
    let file = match open_no_follow(&canonical) {
        Ok(file) => file,
        Err(_) => {
            // When: `open_no_follow` returns Err, the file cannot be opened without following a link.
            return refuse(Refusal::Unreadable, parent);
        }
    };
    let Ok(metadata) = file.metadata() else {
        // When: `fstat` on the descriptor fails, nothing about the file can be trusted.
        return refuse(Refusal::Unreadable, parent);
    };
    if metadata.is_dir() {
        // When: `metadata` describes a directory, nothing runs and nothing descends into it.
        return refuse(Refusal::Directory, parent);
    }
    if !metadata.is_file() {
        // When: `metadata` is a FIFO, socket or device, it is neither a script nor a program.
        return refuse(Refusal::NotRegular, parent);
    }
    let Ok(header) = read_header(&file) else {
        // When: reading the `header` fails, the file cannot be identified and fails closed.
        return refuse(Refusal::Unreadable, parent);
    };
    if has_script_extension(&request.launch_path)
        || has_script_extension(&canonical)
        || header.starts_with(b"#!")
    {
        // When: the extension or a `#!` header marks a script, it is drafted, never executed.
        return Disposition::Draft(request);
    }
    if !is_mach_o(&header) {
        // When: `header` is not Mach-O, today's script path warns and drafts nothing.
        return Disposition::Draft(request);
    }
    if quarantine_present(&file) != Ok(false) {
        // When: `quarantine_present` finds the attribute or fails to read it, never run the file.
        return refuse(Refusal::Quarantined, parent);
    }
    if !passes_execute_check(&canonical) {
        // When: `passes_execute_check` fails for `canonical`, the file is not executable; never add the bit.
        return refuse(Refusal::NotExecutable, parent);
    }
    let Some(cwd) = parent else {
        // When: the canonical path has no `parent` (it is `/`), it cannot be a regular file to run.
        return refuse(Refusal::NotRegular, None);
    };
    let identity = FileIdentity { device: metadata.dev(), inode: metadata.ino() };
    Disposition::Run { program: canonical, cwd, identity }
}

/// The outermost path component ending in `.app` (any case), as a path prefix.
fn outermost_bundle(path: &Path) -> Option<PathBuf> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component);
        let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".app") {
            // When: `name` ends in `.app`, this prefix is the outermost bundle on the path.
            return Some(prefix);
        }
    }
    None
}

/// Open `path` read-only without following a final symlink and without blocking on a FIFO.
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

/// Read up to the first eight bytes of `file`.
fn read_header(file: &File) -> std::io::Result<Vec<u8>> {
    let mut header = Vec::with_capacity(8);
    let reader = file;
    reader.take(8).read_to_end(&mut header)?;
    Ok(header)
}

/// Whether `path` has one of the script extensions (any case).
fn has_script_extension(path: &Path) -> bool {
    path.extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|extension| SCRIPT_EXTENSIONS.contains(&extension.as_str()))
}

/// Whether `header` starts with a thin Mach-O magic, or a fat magic with 1-16 architectures.
fn is_mach_o(header: &[u8]) -> bool {
    let Some(magic_bytes) = header.get(..4) else {
        // When: `header` is shorter than four bytes, it holds no magic number.
        return false;
    };
    let magic =
        u32::from_be_bytes([magic_bytes[0], magic_bytes[1], magic_bytes[2], magic_bytes[3]]);
    if THIN_MACH_O_MAGICS.contains(&magic) {
        // When: `magic` is a thin Mach-O magic in either byte order, the file is a native image.
        return true;
    }
    if !FAT_MACH_O_MAGICS.contains(&magic) {
        // When: `magic` is neither thin nor fat, the file is not Mach-O.
        return false;
    }
    let Some(count_bytes) = header.get(4..8) else {
        // When: a fat `header` lacks its architecture count, it cannot be told from other formats.
        return false;
    };
    let architecture_count =
        u32::from_be_bytes([count_bytes[0], count_bytes[1], count_bytes[2], count_bytes[3]]);
    (1..=MAX_FAT_ARCHITECTURES).contains(&architecture_count)
}

/// Whether `file` carries the quarantine attribute; `Err` for any error but "attribute absent".
fn quarantine_present(file: &File) -> Result<bool, ()> {
    use std::os::fd::AsRawFd;
    let name = CString::new(QUARANTINE_ATTRIBUTE).map_err(|_| ())?;
    let descriptor = file.as_raw_fd();
    let length =
        // SAFETY: `descriptor` stays open while `file` lives, `name` is NUL-terminated; a null buffer of size 0 reads only the length.
        unsafe { libc::fgetxattr(descriptor, name.as_ptr(), std::ptr::null_mut(), 0, 0, 0) };
    if length >= 0 {
        // When: `length` is non-negative, the attribute exists, whatever its value.
        return Ok(true);
    }
    let error = std::io::Error::last_os_error().raw_os_error();
    if error == Some(libc::ENOATTR) {
        // When: `error` is ENOATTR, the file carries no quarantine attribute.
        return Ok(false);
    }
    Err(())
}

/// Whether the current user may execute `path`, by `access(X_OK)`.
fn passes_execute_check(path: &Path) -> bool {
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        // When: `path` contains NUL, it cannot name a file and fails closed.
        return false;
    };
    let status =
        // SAFETY: `c_path` is a NUL-terminated C string that outlives the call; `access` only reads it.
        unsafe { libc::access(c_path.as_ptr(), libc::X_OK) };
    status == 0
}

#[cfg(test)]
#[path = "executable_classify_tests.rs"]
mod executable_classify_tests;
