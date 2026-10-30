//! Validate and retain the executable-adjacent Windows console runtime before portable-pty initializes.

use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{bail, ensure, Context, Result};
use windows::{
    core::{s, HSTRING, PCSTR},
    Win32::{
        Foundation::{FreeLibrary, HMODULE},
        Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE},
        Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        },
        System::LibraryLoader::{
            GetModuleFileNameW, GetModuleHandleW, GetProcAddress, LoadLibraryExW,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
        },
    },
};

const VERSION: &str = "1.25.260930003";
const REQUIRED_ENV: &str = "SONICTERM_REQUIRE_CONPTY";
const REPARSE_ATTRIBUTE: u32 = 0x400;
const DLL_SIZE: u64 = 119_608;
const HOST_SIZE: u64 = 1_089_376;
const DLL_SHA256: &str = "feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a";
const HOST_SHA256: &str = "3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586";

/// Handles prevent replacement of the verified pair for as long as portable-pty may call its cached exports.
struct Runtime {
    _directory: File,
    _library_file: File,
    _host_file: File,
    _module: usize,
}

static RUNTIME: OnceLock<Result<Option<Runtime>, String>> = OnceLock::new();

/// Prepare the pinned private runtime once, or preserve development discovery when no package is present.
pub(crate) fn ensure_ready() -> Result<()> {
    let state = RUNTIME.get_or_init(|| initialize().map_err(|error| format!("{error:#}")));
    match state {
        Ok(_) => Ok(()),
        Err(reason) => bail!("Windows console runtime initialization failed: {reason}"),
    }
}

fn initialize() -> Result<Option<Runtime>> {
    let executable = std::env::current_exe().context("resolve executable directory")?;
    let parent = executable.parent().context("executable has no parent directory")?;
    let directory = parent.join("conpty");
    let required = std::env::var_os(REQUIRED_ENV).is_some_and(|value| value == "1");
    let Some(directory_handle) = open_directory(&directory)? else {
        // When: open_directory finds no runtime, only source-development discovery may continue.
        ensure!(!required, "required packaged ConPTY runtime is missing");
        tracing::info!(kind = "development-discovery", "Windows console runtime selected");
        return Ok(None);
    };
    let library_path = directory.join("conpty.dll");
    let library_file = verify_file(&library_path, DLL_SIZE, DLL_SHA256)?;
    let host_file = verify_file(&directory.join("OpenConsole.exe"), HOST_SIZE, HOST_SHA256)?;
    let expected_path = library_path.canonicalize().context("resolve verified console DLL path")?;
    ensure_no_foreign_module(&HSTRING::from("conpty.dll"), &expected_path)?;
    let module =
        // SAFETY: the exact pinned DLL and directory remain open against replacement; dependencies search only that directory/System32.
        unsafe {
            LoadLibraryExW(
                &HSTRING::from(expected_path.as_os_str()),
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .context("load verified console runtime")?;
    let validated = validate_module(module, &expected_path);
    if let Err(error) = validated {
        // When: validated fails, release only this load reference; no caller has received its exports.
        let _ =
            // SAFETY: module is the reference LoadLibraryExW returned above and is not published on this failure path.
            unsafe { FreeLibrary(module) };
        return Err(error);
    }
    tracing::info!(kind = "packaged", version = VERSION, "Windows console runtime selected");
    // The OnceLock is process-lifetime storage: neither module nor file custody is dropped while upstream exports can be used.
    Ok(Some(Runtime {
        _directory: directory_handle,
        _library_file: library_file,
        _host_file: host_file,
        _module: module.0 as usize,
    }))
}

/// Refuse a module already loaded as `module_name` unless it is the verified file at `expected_path`.
fn ensure_no_foreign_module(module_name: &HSTRING, expected_path: &Path) -> Result<()> {
    let previous =
        // SAFETY: module_name is a live NUL-terminated wide string; this call only queries an existing module.
        unsafe { GetModuleHandleW(module_name) }.ok();
    // A module already loaded by name would be reused by portable-pty, so it must be this exact file.
    if let Some(previous) = previous {
        ensure!(
            module_path(previous)? == expected_path,
            "a different conpty.dll is already loaded"
        );
    }
    Ok(())
}

fn open_directory(path: &Path) -> Result<Option<File>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // When: the runtime directory is NotFound, report absence so development discovery decides.
            return Ok(None);
        }
        Err(error) => {
            // When: any other metadata error occurs, a present-but-unreadable runtime fails startup.
            return Err(error).context("inspect console runtime directory");
        }
    };
    ensure!(
        metadata.is_dir() && metadata.file_attributes() & REPARSE_ATTRIBUTE == 0,
        "console runtime directory must be a regular directory, not a reparse point"
    );
    let directory = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .context("retain console runtime directory")?;
    let opened = directory.metadata()?;
    ensure!(
        opened.is_dir() && opened.file_attributes() & REPARSE_ATTRIBUTE == 0,
        "console runtime directory changed during admission"
    );
    Ok(Some(directory))
}

fn verify_file(path: &Path, expected_size: u64, expected_digest: &str) -> Result<File> {
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .with_context(|| {
            format!(
                "open console runtime member {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            )
        })?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.file_attributes() & REPARSE_ATTRIBUTE == 0,
        "console runtime member must be a regular file, not a reparse point"
    );
    ensure!(metadata.len() == expected_size, "console runtime member size differs from its pin");
    let mut bytes = Vec::with_capacity(expected_size as usize);
    (&mut file).take(expected_size + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 == expected_size, "console runtime member changed while read");
    ensure!(sha256(&bytes)? == expected_digest, "console runtime member hash differs from its pin");
    Ok(file)
}

fn sha256(bytes: &[u8]) -> Result<String> {
    let mut digest = [0u8; 32];
    // SAFETY: the predefined SHA256 provider uses the borrowed input and fixed-size output for this synchronous call only.
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, bytes, &mut digest) }.ok()?;
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn module_path(module: HMODULE) -> Result<PathBuf> {
    let mut path = vec![0u16; 32768];
    let count =
        // SAFETY: module is live and the writable buffer's capacity is supplied by the slice wrapper.
        unsafe { GetModuleFileNameW(Some(module), &mut path) } as usize;
    ensure!(
        count != 0 && count < path.len(),
        "console runtime module path is unavailable or truncated"
    );
    use std::os::windows::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_wide(&path[..count]))
        .canonicalize()
        .context("resolve loaded console runtime path")
}

fn validate_module(module: HMODULE, expected_path: &Path) -> Result<()> {
    ensure!(
        module_path(module)? == expected_path,
        "loaded console runtime differs from verified path"
    );
    for export in [s!("CreatePseudoConsole"), s!("ResizePseudoConsole"), s!("ClosePseudoConsole")] {
        ensure!(has_export(module, export), "console runtime is missing a required API export");
    }
    Ok(())
}

fn has_export(module: HMODULE, export: PCSTR) -> bool {
    // SAFETY: module is retained and export is a static NUL-terminated API name; the address is checked, not called here.
    unsafe { GetProcAddress(module, export) }.is_some()
}

#[cfg(test)]
#[path = "conpty_runtime_tests.rs"]
mod conpty_runtime_tests;
