//! Native macOS `NSMenu` for Sonic Terminal.
//!
//! Top-level submenus (in order): **Sonic / Shell / Edit / View / Help**.
//! Items dispatch to `sonic_core::keymap::Action`s via the
//! [`sonic_shared::menubar_bridge`] queue; the winit loop drains and
//! routes through `App::run_action` — the same path used by keybindings.
//!
//! Help items that point to URLs are opened directly from the AppKit
//! main thread via `NSWorkspace::openURL:` so no new `Action` variant
//! is required.

#![cfg(target_os = "macos")]

use std::path::Path;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem, NSWorkspace};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString, NSURL};

use sonic_core::keymap::Action;
use sonic_shared::menubar_bridge;

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
            menubar_bridge::push_action(action)
        }
        MenuEntry::Url(url) => {
            #[cfg(target_os = "macos")]
            open_url(&url);
            true
        }
    }
}

#[cfg(target_os = "macos")]
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
// Blueprint — pure-data description of the menubar.
//
// Tests assert on this; the AppKit installer also walks it to build
// NSMenuItems, so the two representations cannot drift.
// ---------------------------------------------------------------------

/// A single leaf item in the blueprint.
#[derive(Debug, Clone)]
pub struct Item {
    pub title: &'static str,
    pub key: &'static str,
    pub mods: KeyMods,
    pub binding: Binding,
}

/// Modifier-key shorthand used in the blueprint (translated to
/// `NSEventModifierFlags` at install time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMods {
    None,
    Cmd,
    CmdShift,
    CmdOpt,
}

/// What activating an item does.
#[derive(Debug, Clone)]
pub enum Binding {
    /// Queue an `Action` via the menubar bridge.
    Action(Action),
    /// Open a URL via `NSWorkspace`.
    Url(&'static str),
    /// Bound to a standard AppKit selector; passed through as a string
    /// so the blueprint stays platform-agnostic for unit tests.
    System(&'static str),
    Separator,
}

/// A top-level submenu in the blueprint.
#[derive(Debug, Clone)]
pub struct Submenu {
    pub title: &'static str,
    pub items: Vec<Item>,
}

/// Build the menubar blueprint. Pure: no AppKit calls, safe in tests.
pub fn blueprint() -> Vec<Submenu> {
    use Binding::*;
    use KeyMods::*;

    let sep = || Item { title: "", key: "", mods: None, binding: Separator };

    vec![
        Submenu {
            title: "Sonic",
            items: vec![
                Item {
                    title: "About Sonic",
                    key: "",
                    mods: None,
                    binding: System("orderFrontStandardAboutPanel:"),
                },
                sep(),
                Item {
                    title: "Preferences…",
                    key: ",",
                    mods: Cmd,
                    binding: Action(sonic_core::keymap::Action::OpenPreferences),
                },
                sep(),
                Item { title: "Hide Sonic", key: "h", mods: Cmd, binding: System("hide:") },
                Item {
                    title: "Hide Others",
                    key: "h",
                    mods: CmdOpt,
                    binding: System("hideOtherApplications:"),
                },
                Item {
                    title: "Show All",
                    key: "",
                    mods: None,
                    binding: System("unhideAllApplications:"),
                },
                sep(),
                Item { title: "Quit Sonic", key: "q", mods: Cmd, binding: System("terminate:") },
            ],
        },
        Submenu {
            title: "Shell",
            items: vec![
                Item {
                    title: "New Tab",
                    key: "t",
                    mods: Cmd,
                    binding: Action(sonic_core::keymap::Action::NewTab),
                },
                Item {
                    title: "New Window",
                    key: "n",
                    mods: Cmd,
                    binding: Action(sonic_core::keymap::Action::NewWindow),
                },
                sep(),
                Item {
                    title: "Split Right",
                    key: "d",
                    mods: Cmd,
                    binding: Action(sonic_core::keymap::Action::SplitRight),
                },
                Item {
                    title: "Split Down",
                    key: "d",
                    mods: CmdShift,
                    binding: Action(sonic_core::keymap::Action::SplitDown),
                },
                sep(),
                Item {
                    title: "Close Tab",
                    key: "w",
                    mods: Cmd,
                    binding: Action(sonic_core::keymap::Action::CloseTab),
                },
                Item {
                    title: "Close Pane",
                    key: "w",
                    mods: CmdShift,
                    binding: Action(sonic_core::keymap::Action::ClosePane),
                },
            ],
        },
        Submenu {
            title: "Edit",
            items: vec![
                Item {
                    title: "Copy",
                    key: "c",
                    mods: Cmd,
                    binding: Action(sonic_core::keymap::Action::CopyToClipboard),
                },
                Item {
                    title: "Paste",
                    key: "v",
                    mods: Cmd,
                    binding: Action(sonic_core::keymap::Action::PasteFromClipboard),
                },
                sep(),
                Item {
                    title: "Find…",
                    key: "f",
                    mods: Cmd,
                    binding: Action(sonic_core::keymap::Action::OpenSearch),
                },
                Item {
                    title: "Command Palette",
                    key: "p",
                    mods: CmdShift,
                    binding: Action(sonic_core::keymap::Action::OpenCommandPalette),
                },
            ],
        },
        Submenu {
            title: "View",
            items: vec![
                Item {
                    title: "Toggle Tab Bar",
                    key: "t",
                    mods: CmdShift,
                    binding: Action(sonic_core::keymap::Action::ToggleTabBar),
                },
                Item {
                    title: "Reset Zoom",
                    key: "0",
                    mods: Cmd,
                    binding: Action(sonic_core::keymap::Action::ResetFontSize),
                },
            ],
        },
        Submenu {
            title: "Help",
            items: vec![
                Item {
                    title: "Sonic Help",
                    key: "",
                    mods: None,
                    binding: Url("https://github.com/D0n9X1n/sonic"),
                },
                Item {
                    title: "Report Issue",
                    key: "",
                    mods: None,
                    binding: Url("https://github.com/D0n9X1n/sonic/issues/new"),
                },
            ],
        },
    ]
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
    let menu = NSMenu::new(mtm);
    menu.setTitle(&ns(sm.title));
    for it in &sm.items {
        menu.addItem(&build_item(mtm, it, target));
    }
    container.setSubmenu(Some(&menu));
    container
}

/// Install the Sonic NSMenu as the application's main menu. The
/// `_theme_names` argument is accepted for backward compatibility with
/// existing call sites; the blueprint no longer surfaces themes in the
/// menubar (they live in Preferences).
pub fn install(_theme_names: &[String]) {
    let mtm = MainThreadMarker::new().expect("install_menubar must run on the macOS main thread");
    let app = NSApplication::sharedApplication(mtm);
    let target = MenuTarget::new(mtm);

    let main = NSMenu::new(mtm);
    for sm in blueprint() {
        main.addItem(&build_submenu(mtm, &sm, &target));
    }
    app.setMainMenu(Some(&main));

    // MenuTarget must outlive the menu items that reference it.
    // Leak intentionally — lives for the program's lifetime.
    let _ = Retained::into_raw(target);

    tracing::info!("macOS native menubar installed");
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
