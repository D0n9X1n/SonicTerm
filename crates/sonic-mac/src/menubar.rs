//! Native macOS `NSMenu` for Sonic Terminal.
//!
//! Top-level submenus (in order): **Sonic / Shell / Edit / View / Help**.
//! Items dispatch to `sonic_core::keymap::Action`s via the
//! [`sonic_app::menubar_bridge`] queue; the winit loop drains and
//! routes through `App::run_action` — the same path used by keybindings.
//!
//! Help items that point to URLs are opened directly from the AppKit
//! main thread via `NSWorkspace::openURL:` so no new `Action` variant
//! is required.
//!
//! Shared blueprint + types live in [`sonic_app::menu`]; this file
//! is now the macOS-specific [`PlatformMenu`] implementation only.

#![cfg(target_os = "macos")]

use std::path::Path;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem, NSWorkspace};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString, NSURL};

use sonic_app::menu::{self, PlatformMenu, Sender};
use sonic_core::keymap::Action;

// Re-export shared blueprint types so external integration tests and
// call sites that referenced `menubar::Item` / `Binding` / `KeyMods`
// still compile.
pub use sonic_app::menu::{blueprint, Binding, Item, KeyMods, MenuBlueprint, Submenu};

// ---------------------------------------------------------------------
// Dispatch registry: tag → MenuEntry.
// ---------------------------------------------------------------------

/// Anything an in-process menu item can do when clicked.
#[derive(Debug, Clone)]
pub enum MenuEntry {
    /// Queue a keymap [`Action`] for the next event-loop drain.
    Act(Action),
    /// Open `url` via `NSWorkspace::openURL:` from the AppKit thread.
    Url(String),
}

static ENTRIES: Mutex<Vec<MenuEntry>> = Mutex::new(Vec::new());

fn register(entry: MenuEntry) -> isize {
    let mut v = ENTRIES.lock().expect("menubar entry registry poisoned");
    v.push(entry);
    // 1-based: 0 is AppKit's default tag.
    v.len() as isize
}

fn lookup(tag: isize) -> Option<MenuEntry> {
    let v = ENTRIES.lock().ok()?;
    let idx = (tag as usize).checked_sub(1)?;
    v.get(idx).cloned()
}

#[cfg(test)]
fn reset_registry_for_tests() {
    if let Ok(mut v) = ENTRIES.lock() {
        v.clear();
    }
}

/// Test bridge: register a menu entry from outside the crate without
/// constructing AppKit objects. Returns the assigned tag. Hidden from
/// docs; used only by integration tests under `tests/`.
#[doc(hidden)]
pub fn __test_register(entry: MenuEntry) -> isize {
    register(entry)
}

/// Dispatch the entry registered at `tag`. Public for the test bridge so
/// we can simulate an AppKit click without spinning AppKit.
pub fn dispatch_tag(tag: isize) -> bool {
    let Some(entry) = lookup(tag) else {
        tracing::warn!("SonicMenuTarget: tag {tag} has no registered entry");
        return false;
    };
    match entry {
        MenuEntry::Act(action) => {
            tracing::debug!("menubar dispatch -> {action:?}");
            Sender::new().push(action)
        }
        MenuEntry::Url(url) => {
            open_url(&url);
            true
        }
    }
}

fn open_url(url: &str) {
    // Best-effort: invalid URLs are silently ignored (logged at WARN).
    let nsurl = NSURL::URLWithString(&NSString::from_str(url));
    if let Some(nsurl) = nsurl {
        let _ = MainThreadMarker::new()
            .expect("open_url must run on the macOS main thread (AppKit invariant)");
        let workspace = NSWorkspace::sharedWorkspace();
        workspace.openURL(&nsurl);
    } else {
        tracing::warn!("menubar: ignoring malformed URL {url:?}");
    }
}

// ---------------------------------------------------------------------
// MenuTarget — the Objective-C action receiver.
// ---------------------------------------------------------------------

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "SonicMenuTarget"]
    #[ivars = ()]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        #[unsafe(method(dispatch:))]
        fn dispatch(&self, sender: &NSMenuItem) {
            dispatch_tag(sender.tag());
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

// ---------------------------------------------------------------------
// AppKit installer.
// ---------------------------------------------------------------------

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn flags(m: KeyMods) -> NSEventModifierFlags {
    match m {
        KeyMods::None => NSEventModifierFlags::empty(),
        KeyMods::Cmd => NSEventModifierFlags::Command,
        KeyMods::CmdShift => NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
        KeyMods::CmdOpt => NSEventModifierFlags::Command | NSEventModifierFlags::Option,
    }
}

fn ns_selector_from_str(name: &str) -> Sel {
    match name {
        "orderFrontStandardAboutPanel:" => sel!(orderFrontStandardAboutPanel:),
        "hide:" => sel!(hide:),
        "hideOtherApplications:" => sel!(hideOtherApplications:),
        "unhideAllApplications:" => sel!(unhideAllApplications:),
        "terminate:" => sel!(terminate:),
        other => panic!("unknown system selector in menu blueprint: {other}"),
    }
}

fn build_item(mtm: MainThreadMarker, item: &Item, target: &MenuTarget) -> Retained<NSMenuItem> {
    if matches!(item.binding, Binding::Separator) {
        return NSMenuItem::separatorItem(mtm);
    }
    let nsi = NSMenuItem::new(mtm);
    nsi.setTitle(&ns(item.title));
    nsi.setKeyEquivalent(&ns(item.key));
    nsi.setKeyEquivalentModifierMask(flags(item.mods));
    match &item.binding {
        Binding::Action(a) => {
            let tag = register(MenuEntry::Act(a.clone()));
            unsafe {
                nsi.setTag(tag);
                nsi.setTarget(Some(target));
                nsi.setAction(Some(sel!(dispatch:)));
            }
        }
        Binding::Url(url) => {
            let tag = register(MenuEntry::Url((*url).to_string()));
            unsafe {
                nsi.setTag(tag);
                nsi.setTarget(Some(target));
                nsi.setAction(Some(sel!(dispatch:)));
            }
        }
        Binding::System(name) => unsafe {
            nsi.setAction(Some(ns_selector_from_str(name)));
        },
        Binding::Separator => unreachable!(),
    }
    nsi
}

fn build_submenu(mtm: MainThreadMarker, sm: &Submenu, target: &MenuTarget) -> Retained<NSMenuItem> {
    let container = NSMenuItem::new(mtm);
    container.setTitle(&ns(sm.title));
    let m = NSMenu::new(mtm);
    m.setTitle(&ns(sm.title));
    for it in &sm.items {
        m.addItem(&build_item(mtm, it, target));
    }
    container.setSubmenu(Some(&m));
    container
}

/// macOS [`PlatformMenu`] implementation. The `Sender` is accepted
/// by the trait for symmetry with the Windows impl, but on macOS each
/// click ultimately routes through the same `menubar_bridge` static
/// queue that the `Sender` wraps — so passing a fresh `Sender::new()`
/// produces identical behavior.
#[derive(Debug, Default)]
pub struct MacMenu {
    blueprint: MenuBlueprint,
}

impl MacMenu {
    pub fn new() -> Self {
        Self { blueprint: menu::blueprint() }
    }
}

impl PlatformMenu for MacMenu {
    fn install(&self, _sender: Sender) -> anyhow::Result<()> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("MacMenu::install must run on the macOS main thread"))?;
        let app = NSApplication::sharedApplication(mtm);
        let target = MenuTarget::new(mtm);

        let main = NSMenu::new(mtm);
        for sm in &self.blueprint {
            main.addItem(&build_submenu(mtm, sm, &target));
        }
        app.setMainMenu(Some(&main));

        // MenuTarget must outlive the menu items that reference it.
        // Leak intentionally — lives for the program's lifetime.
        let _ = Retained::into_raw(target);

        tracing::info!("macOS native menubar installed");
        Ok(())
    }
}

/// Install the Sonic NSMenu as the application's main menu. The
/// `_theme_names` argument is accepted for backward compatibility with
/// existing call sites; the blueprint no longer surfaces themes in the
/// menubar (they live in Preferences).
pub fn install(_theme_names: &[String]) {
    if let Err(e) = MacMenu::new().install(Sender::new()) {
        tracing::error!("install_menubar: {e}");
    }
}

// ---------------------------------------------------------------------
// Theme list helper (kept for callers that still scan).
// ---------------------------------------------------------------------

pub fn scan_themes(themes_dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(themes_dir) else {
        tracing::warn!("menubar: cannot read theme dir {themes_dir:?}");
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("toml") {
                p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

// NOTE (CLAUDE.md §5): Tests stay inline. They reach into private
// `register`/`lookup`/`scan_themes`/`reset_registry_for_tests` and the
// surface is small + macOS-only — adding pub-with-doc-hidden shims for
// each is more disruptive than keeping the inline block.
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn scan_themes_returns_sorted_basenames() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("dracula.toml"), "name = \"x\"\n").unwrap();
        fs::write(dir.path().join("nord.toml"), "name = \"x\"\n").unwrap();
        fs::write(dir.path().join("README.md"), "ignored").unwrap();
        let names = scan_themes(dir.path());
        assert_eq!(names, vec!["dracula".to_string(), "nord".to_string()]);
    }

    #[test]
    fn scan_themes_missing_dir_is_empty_not_panic() {
        let names = scan_themes(Path::new("/no/such/path/should/exist/here"));
        assert!(names.is_empty());
    }

    #[test]
    fn register_and_lookup_round_trips() {
        reset_registry_for_tests();
        let tag = register(MenuEntry::Act(Action::NewTab));
        assert!(tag >= 1);
        let got = lookup(tag).expect("registered tag should resolve");
        assert!(matches!(got, MenuEntry::Act(Action::NewTab)));
        assert!(lookup(0).is_none());
        assert!(lookup(-1).is_none());
    }
}
