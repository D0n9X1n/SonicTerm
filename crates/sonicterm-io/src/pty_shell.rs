use super::*;

pub(super) fn apply_clean_e2e_environment(builder: &mut CommandBuilder, clean_e2e: bool) {
    if clean_e2e {
        // Suppress shell hooks without replacing HOME or unrelated environment.
        builder.env_remove("ENV");
        builder.env_remove("BASH_ENV");
    }
}

fn default_shell() -> String {
    default_shell_program()
}

/// Resolve the shell to spawn: an explicit, non-empty `[terminal] shell`
/// override wins; otherwise fall back to the platform auto-detect.
/// Pure so it can be unit-tested without the live filesystem/PATH.
pub(super) fn resolve_spawn_shell(override_shell: Option<&str>) -> String {
    match override_shell {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => default_shell(),
    }
}

const DEFAULT_LANG_UTF8_LOCALE: &str = "en_US.UTF-8";
const DEFAULT_LC_CTYPE_UTF8_LOCALE: &str = "UTF-8";

/// Return startup arguments for the selected shell.
///
/// Production macOS shells are login shells so `/etc/zprofile` can run
/// `path_helper`, matching Terminal.app/iTerm2/WezTerm PATH behavior. Clean
/// E2E mode intentionally bypasses profiles for deterministic fixtures.
#[doc(hidden)]
pub fn shell_startup_args(shell_path: &str, opts: ShellSpawnOpts) -> Vec<String> {
    if opts.clean_e2e {
        clean_e2e_args(shell_path)
    } else {
        // When: `clean_e2e` is false, preserve the selected shell's normal interactive startup.
        interactive_shell_args(shell_path)
    }
}

#[cfg(target_os = "macos")]
fn apply_terminal_locale_env(builder: &mut CommandBuilder) {
    let lc_all = builder.get_env("LC_ALL").and_then(|v| v.to_str());
    let lc_ctype = builder.get_env("LC_CTYPE").and_then(|v| v.to_str());
    let lang = builder.get_env("LANG").and_then(|v| v.to_str());

    if should_apply_utf8_locale_fallback(lc_all, lc_ctype, lang) {
        if is_empty_env(lang) {
            builder.env("LANG", DEFAULT_LANG_UTF8_LOCALE);
        }
        builder.env("LC_CTYPE", DEFAULT_LC_CTYPE_UTF8_LOCALE);
    }
}

#[cfg(not(target_os = "macos"))]
fn apply_terminal_locale_env(_builder: &mut CommandBuilder) {}

/// Reports whether macOS needs a UTF-8 locale fallback without overriding `LC_ALL`.
#[doc(hidden)]
pub fn should_apply_utf8_locale_fallback(
    lc_all: Option<&str>,
    lc_ctype: Option<&str>,
    lang: Option<&str>,
) -> bool {
    if !is_empty_env(lc_all) {
        // When: `lc_all` is explicit, it overrides lower-priority locale variables and must be preserved.
        return false;
    }
    !is_utf8_locale(lc_ctype) && !is_utf8_locale(lang)
}

/// Returns the `LANG` value used when no locale variables are set.
#[doc(hidden)]
pub const fn default_lang_utf8_locale() -> &'static str {
    DEFAULT_LANG_UTF8_LOCALE
}

/// Returns the UTF-8 `LC_CTYPE` value used by the macOS fallback.
#[doc(hidden)]
pub const fn default_lc_ctype_utf8_locale() -> &'static str {
    DEFAULT_LC_CTYPE_UTF8_LOCALE
}

fn is_empty_env(value: Option<&str>) -> bool {
    value.map(str::trim).unwrap_or_default().is_empty()
}

fn is_utf8_locale(value: Option<&str>) -> bool {
    let Some(value) = value else {
        // When: `value` absent means no locale spelling can advertise UTF-8.
        return false;
    };
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    normalized.contains("utf-8") || normalized.contains("utf8")
}

#[cfg(target_os = "macos")]
fn interactive_shell_args(shell_path: &str) -> Vec<String> {
    let name = shell_file_name(shell_path);
    match name.as_str() {
        "zsh" | "zsh.exe" | "tcsh" | "csh" => vec!["-l".to_string()],
        "bash" | "bash.exe" | "fish" | "fish.exe" => vec!["--login".to_string()],
        _ => Vec::new(),
    }
}

#[cfg(target_os = "windows")]
fn interactive_shell_args(shell_path: &str) -> Vec<String> {
    let name = shell_file_name(shell_path);
    match name.as_str() {
        "pwsh.exe" | "powershell.exe" | "pwsh" | "powershell" => vec![
            "-NoLogo".to_string(),
            "-NoExit".to_string(),
            "-Command".to_string(),
            format!("[Console]::InputEncoding=[System.Text.UTF8Encoding]::new($false); [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $OutputEncoding=[System.Text.UTF8Encoding]::new($false); chcp 65001 > $null; & {{\n{}\n}}", include_str!("../../../scripts/powershell-integration.ps1")),
        ],
        _ => Vec::new(),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn interactive_shell_args(_shell_path: &str) -> Vec<String> {
    Vec::new()
}

#[cfg(target_os = "windows")]
fn default_shell_program() -> String {
    resolve_windows_default_shell_with(
        || path_lookup("pwsh.exe"),
        registered_pwsh,
        windowsapps_store_pwsh,
        || path_lookup("powershell.exe"),
    )
}

#[cfg(target_os = "windows")]
fn resolve_windows_default_shell_with<PathPwsh, RegisteredPwsh, StorePwsh, LegacyPwsh>(
    path_pwsh: PathPwsh,
    registered_pwsh: RegisteredPwsh,
    store_pwsh: StorePwsh,
    legacy_pwsh: LegacyPwsh,
) -> String
where
    PathPwsh: FnOnce() -> Option<String>,
    RegisteredPwsh: FnOnce() -> Option<String>,
    StorePwsh: FnOnce() -> Option<String>,
    LegacyPwsh: FnOnce() -> Option<String>,
{
    path_pwsh()
        .or_else(registered_pwsh)
        .or_else(store_pwsh)
        .or_else(legacy_pwsh)
        .unwrap_or_else(|| "cmd.exe".to_string())
}

#[cfg(target_os = "windows")]
fn registered_pwsh() -> Option<String> {
    use std::{ffi::c_void, os::windows::ffi::OsStringExt};

    use windows::{
        core::{w, PCWSTR},
        Win32::{
            Foundation::ERROR_SUCCESS,
            System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ},
        },
    };

    const MAX_PATH_BYTES: u32 = 64 * 1024 + 2;
    let mut bytes = 0u32;
    let status =
        // SAFETY: this size query passes no data buffer and gives `bytes` as writable output storage.
        unsafe {
            RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\pwsh.exe"),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
            )
        };
    if status != ERROR_SUCCESS || !(2..=MAX_PATH_BYTES).contains(&bytes) {
        // When: `status` failed or `bytes` is implausible, do not allocate from untrusted registry size data.
        return None;
    }

    let mut value = vec![0u16; (bytes as usize).div_ceil(2)];
    let status =
        // SAFETY: `value` is writable for the byte count returned by the prior successful size query.
        unsafe {
            RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\pwsh.exe"),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(value.as_mut_ptr().cast::<c_void>()),
            Some(&mut bytes),
            )
        };
    if status != ERROR_SUCCESS {
        // When: the registry value read `status` failed, so `value` does not contain a usable path.
        return None;
    }

    let units = (bytes as usize / 2).min(value.len());
    let end = value[..units].iter().position(|&unit| unit == 0).unwrap_or(units);
    let path = PathBuf::from(std::ffi::OsString::from_wide(&value[..end]));
    path.is_file().then(|| path.to_string_lossy().into_owned())
}

/// Probe the Microsoft Store package directory for a real, executable
/// `pwsh.exe` (PowerShell 7). Returns the highest-versioned one, or `None`.
///
/// `BUILTIN\Users` has read+execute on `C:\Program Files\WindowsApps`, so a
/// normal (non-elevated) process can enumerate `Microsoft.PowerShell_*`.
/// Honors the `SONICTERM_ALLOW_WINDOWSAPPS_SHELL` escape hatch only to the
/// extent that this real package path is always allowed (it works under
/// ConPTY, unlike the per-user alias stub).
#[cfg(target_os = "windows")]
fn windowsapps_store_pwsh() -> Option<String> {
    let program_files = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    let windowsapps = program_files.join("WindowsApps");
    let entries = std::fs::read_dir(&windowsapps).ok()?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = entry.file_name();
        let lname = name.to_string_lossy().to_ascii_lowercase();
        // Match the PowerShell package family; skip the `_neutral_~_`
        // resource package (no real exe). The arch'd package
        // (`Microsoft.PowerShell_<ver>_x64__<pub>`) carries `pwsh.exe`.
        if !lname.starts_with("microsoft.powershell_") {
            // When: `lname` is outside the PowerShell package family, it cannot contain the target executable.
            continue;
        }
        let pwsh = dir.join("pwsh.exe");
        if pwsh.is_file() {
            candidates.push(pwsh);
        }
    }
    pick_highest_pwsh(&candidates).map(|p| p.to_string_lossy().to_string())
}

/// Pure selector: pick the highest-versioned `pwsh.exe` from candidate Store
/// package paths. Versions are embedded in the parent dir name
/// (`Microsoft.PowerShell_7.6.2.0_x64__...`); compare them numerically so
/// `7.10` sorts above `7.9`. Falls back to lexical path order when no version
/// parses. Returns `None` for an empty list.
#[cfg(target_os = "windows")]
fn pick_highest_pwsh(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .max_by(|a, b| {
            let va = store_pkg_version(a);
            let vb = store_pkg_version(b);
            va.cmp(&vb).then_with(|| a.as_os_str().cmp(b.as_os_str()))
        })
        .cloned()
}

/// Extract the `[major, minor, patch, build]` version from a Store package
/// `pwsh.exe` path's parent dir name (`Microsoft.PowerShell_<ver>_<arch>__...`).
/// Returns all-zero when it can't be parsed, so unparseable entries sort below
/// real ones.
#[cfg(target_os = "windows")]
fn store_pkg_version(pwsh_path: &Path) -> [u64; 4] {
    let dir = pwsh_path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    // `Microsoft.PowerShell_7.6.2.0_x64__8wekyb3d8bbwe`
    let after = dir.split('_').nth(1).unwrap_or("");
    let mut out = [0u64; 4];
    for (i, part) in after.split('.').take(4).enumerate() {
        out[i] = part.parse().unwrap_or(0);
    }
    out
}

#[cfg(unix)]
fn resolve_unix_default_shell_with(
    environment_shell: Option<&str>,
    passwd_shell: Option<&str>,
    executable: impl Fn(&Path) -> bool,
) -> String {
    environment_shell
        .into_iter()
        .chain(passwd_shell)
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .find(|candidate| executable(Path::new(candidate)))
        .unwrap_or("/bin/sh")
        .to_string()
}

#[cfg(unix)]
fn unix_shell_is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(unix)]
fn passwd_shell() -> Option<String> {
    use std::ffi::CStr;

    let mut capacity = 16 * 1024;
    loop {
        let mut record: libc::passwd =
            // SAFETY: zeroed `passwd` is writable output storage for `getpwuid_r`.
            unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0u8; capacity];
        let status =
            // SAFETY: `record`, `buffer`, and `result` remain writable for this call; `getuid` supplies the current real user id.
            unsafe {
                libc::getpwuid_r(
                    libc::getuid(),
                    &mut record,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut result,
                )
            };
        if status == libc::ERANGE && capacity < 1024 * 1024 {
            // When: `status` is `ERANGE`, retry with bounded larger storage instead of reading a partial record.
            capacity *= 2;
            continue;
        }
        if status != 0 || result.is_null() || record.pw_shell.is_null() {
            // When: lookup `status`, `result`, or `pw_shell` is invalid, no trustworthy passwd shell is available.
            return None;
        }
        return
            // SAFETY: successful `getpwuid_r` stores a NUL-terminated `pw_shell` pointer into the live `buffer`.
            unsafe { CStr::from_ptr(record.pw_shell) }
                .to_str()
                .ok()
                .map(str::to_string);
    }
}

#[cfg(unix)]
fn default_shell_program() -> String {
    let environment_shell = std::env::var("SHELL").ok();
    let passwd_shell = passwd_shell();
    resolve_unix_default_shell_with(
        environment_shell.as_deref(),
        passwd_shell.as_deref(),
        unix_shell_is_executable,
    )
}

#[cfg(all(not(unix), not(target_os = "windows")))]
fn default_shell_program() -> String {
    "/bin/sh".to_string()
}

#[cfg(target_os = "windows")]
fn path_lookup(name: &str) -> Option<String> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 && candidate.is_file() {
        // When: `candidate` is an explicit existing path, return it without consulting PATH.
        return Some(candidate.to_string_lossy().to_string());
    }
    let path = std::env::var_os("PATH")?;
    let allow_windowsapps =
        std::env::var("SONICTERM_ALLOW_WINDOWSAPPS_SHELL").map(|v| v == "1").unwrap_or(false);
    std::env::split_paths(&path)
        .map(|dir: PathBuf| dir.join(name))
        .find(|candidate| {
            if !candidate.is_file() {
                // When: `candidate` is not a file, PATH lookup cannot spawn it.
                return false;
            }
            // skip Microsoft Store WindowsApps stubs for `pwsh.exe` /
            // `powershell.exe`. The App Execution Alias produces zero output
            // under ConPTY when spawned bare, so the e2e gates silently fail.
            // Escape hatch: SONICTERM_ALLOW_WINDOWSAPPS_SHELL=1 to opt back in.
            let lname = name.to_ascii_lowercase();
            let is_powershell = lname.ends_with("pwsh.exe") || lname.ends_with("powershell.exe");
            if is_powershell && !allow_windowsapps {
                // When: `is_powershell` and aliases are disallowed, reject only a per-user WindowsApps stub.
                let lpath = candidate.to_string_lossy().to_lowercase();
                // Skip only per-user App Execution Alias stubs. The real
                // Microsoft Store PowerShell package also lives under a
                // WindowsApps directory (usually `C:\Program Files\WindowsApps\
                // Microsoft.PowerShell_*\pwsh.exe`) and works correctly under
                // ConPTY; skipping every `\WindowsApps\` path made SonicTerm
                // fall back to Windows PowerShell 5.1, whose PSReadLine redraw
                // path emits literal `?` bytes for CJK edits.
                if is_windowsapps_alias_stub_path(&lpath) {
                    // When: `lpath` matched the per-user alias stub path, so skip it.
                    return false;
                }
            }
            true
        })
        .map(|path| path.to_string_lossy().to_string())
}

#[cfg(target_os = "windows")]
fn is_windowsapps_alias_stub_path(lowercase_path: &str) -> bool {
    lowercase_path.contains("\\appdata\\local\\microsoft\\windowsapps\\")
}

#[doc(hidden)]
/// The `TERM_PROGRAM_VERSION` to advertise for a given `TERM_PROGRAM`.
///
/// Running as ourselves, this is SonicTerm's own version. But `term_program`
/// is configurable precisely so a user can present as a terminal that tools
/// already recognise, and a name/version pair has to be internally consistent
/// to be useful: programs gate features on the version *of the terminal the
/// name claims*.
///
/// WezTerm versions its releases by datestamp, and consumers compare those
/// lexically. A semver string sorts below any datestamp, so sending
/// SonicTerm's `1.2.0` under WezTerm's name fails every such gate — the tool
/// takes its WezTerm branch on the name, then disables the features it just
/// decided the terminal was too old for. That is worse than either identity
/// alone.
///
/// The advertised datestamp is the release that introduced the capabilities
/// SonicTerm actually implements, not a moving "now": claiming to be newer
/// than we are would invite gates for features we do not have.
fn term_program_version(term_program: &str) -> &'static str {
    match term_program {
        "WezTerm" => WEZTERM_ADVERTISED_VERSION,
        _ => env!("CARGO_PKG_VERSION"),
    }
}

/// Datestamp advertised when presenting as WezTerm.
///
/// Above the thresholds consumers test for modern capabilities — notably
/// styled underlines, which is the gate that otherwise makes editors probe
/// with a DCS query instead of enabling the feature outright.
const WEZTERM_ADVERTISED_VERSION: &str = "20230712-072601";

/// Applies terminal capability, identity, and locale variables to a child command.
pub fn apply_child_pty_env(builder: &mut CommandBuilder, term_program: &str) {
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");
    // Identify the terminal to programs that branch on TERM_PROGRAM
    // (e.g. Copilot CLI, shells, prompt frameworks). Mirrors iTerm2 /
    // WezTerm, which set TERM_PROGRAM + TERM_PROGRAM_VERSION.
    builder.env("TERM_PROGRAM", term_program);
    builder.env("TERM_PROGRAM_VERSION", term_program_version(term_program));
    apply_terminal_locale_env(builder);
}

#[cfg(test)]
#[path = "pty_shell_tests.rs"]
mod pty_shell_tests;
