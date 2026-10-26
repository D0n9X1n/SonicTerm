use super::*;

/// Native resize plus the last size it accepted, under one lock.
struct ResizeState {
    apply: Box<dyn FnMut(u16, u16) -> Result<()> + Send>,
    last_applied: Option<(u16, u16)>,
}

/// Build the pty resize callback over the native `apply`, skipping repeat sizes.
///
/// The returned callback holds the `ResizeState` lock across `apply`, so native
/// resizes are serialized and the stored size cannot disagree with the pty.
pub(super) fn resize_callback(
    apply: Box<dyn FnMut(u16, u16) -> Result<()> + Send>,
) -> Box<dyn Fn(u16, u16) -> Result<()> + Send + Sync> {
    let state = Mutex::new(ResizeState { apply, last_applied: None });
    Box::new(move |cols, rows| {
        if cols == 0 || rows == 0 {
            // When: `cols` or `rows` is zero, refuse before `apply` and before `last_applied` moves.
            return Err(anyhow::Error::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing pty resize to {cols}x{rows}"),
            )));
        }
        let mut state = state.lock();
        if state.last_applied == Some((cols, rows)) {
            // When: the request equals `last_applied`, skip a native reflow that would change nothing.
            return Ok(());
        }
        (state.apply)(cols, rows)?;
        state.last_applied = Some((cols, rows));
        Ok(())
    })
}

#[cfg(test)]
#[path = "pty_resize_tests.rs"]
mod pty_resize_tests;
