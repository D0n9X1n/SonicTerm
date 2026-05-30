//! Windows chrome is native Win11 chrome.
//!
//! Sonic does not subclass or draw the caption area on Windows: the OS owns
//! titlebar drag, snap layouts, and min/max/close buttons. The app only paints
//! the client-area terminal grid and bottom tab bar.

#![cfg(target_os = "windows")]
