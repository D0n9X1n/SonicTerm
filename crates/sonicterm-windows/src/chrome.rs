//! Windows window chrome.
//!
//! Terminal windows are undecorated and SonicTerm draws its own titlebar band with
//! minimise, maximise/restore and close on the right; moves and edge resizes go
//! through winit's native drag calls. `sonicterm-app` subclasses each window with
//! caption buttons so the Windows 11 snap layouts flyout shows on maximise.

#![cfg(target_os = "windows")]
