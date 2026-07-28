//! Minimal Windows compile probe for the Sonic-owned ConPTY seam.
//!
//! This is not a production backend. It references the Win32 symbols the
//! native ConPTY transport owner will call that are reachable through the
//! workspace `windows` features already enabled at the frozen base SHA, so a
//! cross-compile (`cargo check -p sonicterm-io --target x86_64-pc-windows-msvc`)
//! objectively proves the HPCON-ownership + cancellation seam resolves today.
//!
//! Symbols that require features not yet enabled are deliberately *not*
//! referenced here so the probe stays green on the current manifest; their
//! gating is itself corroborated at compile time. For example `CreateProcessW`
//! is gated behind `Win32_Security`, which
//! [`super::WIN_FEATURE_REQUIREMENTS`] lists with `already_enabled = false`;
//! referencing it fails to resolve until the integrator adds that feature in
//! the production WP-PTY package.
#![allow(dead_code)]

use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, HPCON,
};
use windows::Win32::System::Threading::InitializeProcThreadAttributeList;
use windows::Win32::System::IO::CancelSynchronousIo;

/// Reference the ConPTY ownership + cancellation symbols so the seam is proven
/// to resolve at compile time against the current workspace `windows`
/// features. Never called — reachability is the whole point.
fn _pty_backend_v1_symbol_probe() {
    // HPCON ownership + explicit lifecycle (create / resize / close).
    let _ = CreatePseudoConsole;
    let _ = ResizePseudoConsole;
    let _ = ClosePseudoConsole;
    let _: Option<HPCON> = None;
    // Level-triggered cancellation of a thread's pending synchronous console IO.
    let _ = CancelSynchronousIo;
    // Pseudoconsole thread-attribute wiring for the owned child spawn. The
    // spawn call itself (CreateProcessW) needs Win32_Security, an enumerated
    // to-add feature, so it is intentionally not referenced here.
    let _ = InitializeProcThreadAttributeList;
}
