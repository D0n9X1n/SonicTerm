use super::*;

/// Duplicate the Unix PTY master descriptor into an owned writable `File`,
/// whose close is silent. A master exposing no descriptor is an error, never a
/// fallback to a writer that injects bytes.
#[cfg(unix)]
fn unix_master_writer_file(master: &dyn portable_pty::MasterPty) -> Result<std::fs::File> {
    let raw = master.as_raw_fd().ok_or_else(|| {
        anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "unix pty master exposed no descriptor to duplicate for input",
        ))
    })?;
    let borrowed = {
        // SAFETY: `raw` is owned and kept open by `master`, borrowed for this
        // whole call, and only duplicated here — never closed.
        unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) }
    };
    // Duplicates via `F_DUPFD_CLOEXEC`: the new descriptor carries its own
    // FD_CLOEXEC, while file-status flags stay shared with the master.
    Ok(std::fs::File::from(borrowed.try_clone_to_owned()?))
}

/// Build the master-side input writer for a freshly opened PTY. A writer's
/// destructor is part of the child's input stream, so one seam decides it per
/// platform: `portable-pty`'s Unix writer writes a newline and `VEOF` when
/// dropped, delivering input no source produced.
#[cfg(unix)]
pub(super) fn pty_writer(master: &dyn portable_pty::MasterPty) -> Result<Box<dyn Write + Send>> {
    Ok(Box::new(unix_master_writer_file(master)?))
}

/// Build the master-side input writer for a freshly opened PTY. Non-Unix keeps
/// `portable-pty`'s writer, whose ConPTY destructor writes nothing.
#[cfg(not(unix))]
pub(super) fn pty_writer(master: &dyn portable_pty::MasterPty) -> Result<Box<dyn Write + Send>> {
    master.take_writer()
}

#[cfg(test)]
#[path = "pty_writer_file_tests.rs"]
mod pty_writer_file_tests;
