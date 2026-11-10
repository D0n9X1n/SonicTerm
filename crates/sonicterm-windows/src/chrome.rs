//! Windows window chrome.
//!
//! Terminal windows are undecorated and SonicTerm draws its own titlebar band with
//! minimise, maximise/restore and close on the right; moves and edge resizes go
//! through winit's native drag calls. The window is not subclassed, so the Windows
//! 11 snap layouts flyout is not offered on the maximise button.

#![cfg(target_os = "windows")]
