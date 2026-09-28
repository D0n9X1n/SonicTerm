//! Linux path probes and direct-open. Classification follows symlinks and rejects special
//! entries; file selection calls the file manager's `ShowItems` over D-Bus within five seconds;
//! directory navigation revalidates the opened descriptor and hands it to the OpenURI portal,
//! falling back to a fixed `xdg-open` only when the portal is unavailable.

use super::*;

use super::unix::CommandSpec;
#[cfg(target_os = "linux")]
use super::unix::{classify_followed_target, run_command};

#[cfg(target_os = "linux")]
pub(super) fn classify_local_target(path: &Path) -> PathOpenDecision {
    classify_reveal_identity(path)
}

#[cfg(target_os = "linux")]
fn classify_reveal_identity(path: &Path) -> PathOpenDecision {
    match classify_followed_target(path) {
        Ok(kind) => PathOpenDecision::Openable(kind),
        Err(decision) => decision,
    }
}

#[cfg(target_os = "linux")]
pub(super) fn reveal_native_file(path: &Path) -> io::Result<()> {
    use std::future::Future;
    let operation = async {
        let connection = ashpd::zbus::connection::Builder::session()
            .map_err(io::Error::other)?
            .method_timeout(std::time::Duration::from_secs(5))
            .build()
            .await
            .map_err(io::Error::other)?;
        let uri = local_file_uri(path)?;
        connection
            .call_method(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                Some("org.freedesktop.FileManager1"),
                "ShowItems",
                &(vec![uri], ""),
            )
            .await
            .map_err(io::Error::other)?;
        Ok(())
    };
    let mut operation = std::pin::pin!(operation);
    let mut deadline = std::pin::pin!(async_io::Timer::after(std::time::Duration::from_secs(5)));
    async_io::block_on(std::future::poll_fn(|context| {
        if let std::task::Poll::Ready(result) = operation.as_mut().poll(context) {
            // When: operation completed, preserve success or denial without another file-manager request.
            return std::task::Poll::Ready(result);
        }
        if deadline.as_mut().poll(context).is_ready() {
            // When: deadline elapsed, bound connection setup as well as the D-Bus method call.
            return std::task::Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "file manager selection timed out",
            )));
        }
        std::task::Poll::Pending
    }))
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn local_file_uri(path: &Path) -> io::Result<String> {
    let path = path
        .to_str()
        .filter(|text| text.starts_with('/') && !text.starts_with("//"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "nonlocal reveal path"))?;
    let mut uri = String::from("file://");
    use std::fmt::Write;
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            uri.push(char::from(byte));
        } else {
            // When: byte is outside URI path-safe ASCII, encode it so filename delimiters remain data.
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    Ok(uri)
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn with_opened_target<T>(
    path: &Path,
    open: impl FnOnce(&mut std::fs::File) -> io::Result<T>,
) -> io::Result<T> {
    #[cfg(target_os = "linux")]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            // Links are followed; callers check the opened descriptor's type before using it.
            .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?
    };
    #[cfg(not(target_os = "linux"))]
    let mut file = std::fs::File::open(path)?;
    open(&mut file)
}

#[cfg(target_os = "linux")]
fn classify_linux_file(file: &mut std::fs::File) -> io::Result<PathOpenDecision> {
    let metadata = file.metadata()?;
    if metadata.is_dir() {
        // When: `metadata.is_dir()` proves directory identity, preserve that kind for activation-time revalidation.
        return Ok(PathOpenDecision::Openable(PathKind::Directory));
    }
    if !metadata.is_file() {
        // When: `metadata.is_file()` is false after directory rejection, block sockets, devices, and other special entries.
        return Ok(PathOpenDecision::Blocked);
    }
    Ok(PathOpenDecision::Openable(PathKind::File))
}

#[cfg(target_os = "linux")]
fn linux_portal_unavailable(error: &ashpd::Error) -> bool {
    use ashpd::zbus;

    match error {
        ashpd::Error::PortalNotFound(_) => true,
        ashpd::Error::Zbus(
            zbus::Error::Address(_)
            | zbus::Error::Handshake(_)
            | zbus::Error::InputOutput(_)
            | zbus::Error::InterfaceNotFound
            | zbus::Error::Unsupported,
        ) => true,
        ashpd::Error::Zbus(zbus::Error::FDO(error)) => matches!(
            error.as_ref(),
            zbus::fdo::Error::ServiceUnknown(_)
                | zbus::fdo::Error::NameHasNoOwner(_)
                | zbus::fdo::Error::NoServer(_)
                | zbus::fdo::Error::Disconnected(_)
        ),
        _ => false,
    }
}

#[cfg(target_os = "linux")]
pub(super) fn open_native_path(path: &Path, expected_decision: PathOpenDecision) -> io::Result<()> {
    use ashpd::desktop::open_uri::OpenFileRequest;

    let PathOpenDecision::Openable(expected_kind @ PathKind::Directory) = expected_decision else {
        // When: expected_decision is not a directory, file selection must use the file-manager reveal API.
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsupported Linux action"));
    };
    let portal = with_opened_target(path, |file| {
        if classify_linux_file(file)? != PathOpenDecision::Openable(expected_kind) {
            // When: `classify_linux_file` differs from `expected_kind`, reject identity or type changes before portal handoff.
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "changed or blocked Linux target",
            ));
        }
        async_io::block_on(async {
            let request = match OpenFileRequest::default()
                .writeable(false)
                .ask(false)
                .send_file(file)
                .await
            {
                Ok(request) => request,
                Err(error) if linux_portal_unavailable(&error) => {
                    // When: `linux_portal_unavailable` accepts `error`, signal the narrowly permitted fixed-path fallback.
                    return Err(io::Error::new(io::ErrorKind::NotFound, error.to_string()));
                }
                Err(error) => {
                    // When: `send_file` returns another `error`, preserve it and never bypass portal rejection with a fallback.
                    return Err(io::Error::other(error.to_string()));
                }
            };
            request.response().map_err(|error| io::Error::other(error.to_string()))
        })
    });
    match portal {
        Ok(()) => {
            // When: `portal` succeeds, the target was submitted once and no fallback may run.
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // When: portal `error.kind()` is `NotFound`, try only the fixed executable fallback after revalidation.
        }
        Err(error) => {
            // When: `portal` fails for any other reason, return `error` without risking a second open or bypass.
            return Err(error);
        }
    }

    let spec = linux_xdg_open_spec(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "xdg-open unavailable"))?;
    with_opened_target(path, |file| {
        if classify_linux_file(file)? != PathOpenDecision::Openable(expected_kind) {
            // When: fallback `classify_linux_file` no longer returns `expected_kind`, reject a raced or reclassified target.
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "changed or blocked Linux target",
            ));
        }
        run_command(spec)
    })
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn linux_xdg_open_spec(path: &Path) -> Option<CommandSpec> {
    let target = path.to_str()?;
    if !target.starts_with('/') {
        // When: `target` lacks an absolute POSIX root, never pass process-relative state to `xdg-open`.
        return None;
    }
    let program = ["/usr/bin/xdg-open", "/bin/xdg-open"]
        .into_iter()
        .find(|candidate| Path::new(candidate).is_file())?;
    Some(CommandSpec { program: PathBuf::from(program), args: vec![target.to_string()] })
}
