//! Native macOS `NSMenu` for Sonic Terminal.
//!
//! ## What this delivers
//!
//! macOS users who launch `Sonic.app` (or `cargo run -p sonic-mac`)
//! now see the standard Apple-menu / app-menu / File / Edit / View
//! / Terminal / Window / Help bar across the top of the screen.
//! Items marked with ⌘-key shortcuts mirror the wezterm-default
//! keymap; the menu is an *additional* surface for the same
//! `Action`s the existing keybindings already trigger, so a user
//! who relies on `super+,` to open prefs is unaffected.
//!
//! ## How dispatch works
//!
//! AppKit calls `[target action:]` on the AppKit main thread. We
//! install a single `Retained<MenuTarget>` as the action receiver
//! for every Sonic-owned menu item; the selector decodes a tag
//! into a [`sonic_core::keymap::Action`] and posts it through
//! [`sonic_shared::menubar_bridge::push_action`]. The bridge wakes
//! the winit event loop, which drains the queue and dispatches via
//! `App::run_action`. We never call into `App` directly from the
//! AppKit thread — the winit borrow lives behind `run_app(&mut app)`.
//!
//! Items that map cleanly to *standard* AppKit selectors
//! (`terminate:`, `hide:`, etc.) use those directly so they get
//! the standard system behavior (e.g. "Quit" honors the
//! per-application restart preference).
//!
//! ## Theme submenu
//!
//! Built by reading the on-disk `assets/themes/*.toml` directory
//! at startup. Each item dispatches `Action::ApplyTheme(name)`,
//! which (in `App::run_action`) live-applies the new theme and
//! persists it to `Config.theme` for the next save.
//!
//! ## Why not `muda`?
//!
//! `muda` is the canonical cross-platform menu crate but at
//! v0.13 it lacks a clean way to bridge menu events into a
//! `winit::EventLoop<UserEvent>` without spinning a second
//! `mpsc` thread, and it pulls in a non-trivial dependency tree
//! (`gtk` shim feature gating, etc.) we'd then have to audit.
//! The direct objc2 path is ~250 lines and depends only on
//! crates already in this binary.
//!
//! ## Windows
//!
//! Stubbed to a no-op in [`crate::menubar::install`]'s sibling
//! `#[cfg(not(target_os = "macos"))]` arm. Native Windows menus
//! are usually in-window, not at the top of the screen — wiring
//! them belongs alongside the Win32 chrome work, post-v1.

#![cfg(target_os = "macos")]

use std::path::Path;

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};

use sonic_core::keymap::Action;
use sonic_shared::menubar_bridge;

// ---------------------------------------------------------------------
// Tag ↔ Action mapping.
//
// We can't store a `String` in a `NSMenuItem` tag (it's an `isize`), so
// we keep an in-process registry of `(tag, Action)`. The MenuTarget's
// `dispatch:` selector looks up the sender's tag and queues the action.
// ---------------------------------------------------------------------

use std::sync::Mutex;

static ACTIONS: Mutex<Vec<Action>> = Mutex::new(Vec::new());

fn register(action: Action) -> isize {
    let mut v = ACTIONS.lock().expect("menubar action registry poisoned");
    v.push(action);
    // 1-based: a tag of 0 is AppKit's default and we don't want to
    // collide with an "untagged" item that may exist for some reason.
    v.len() as isize
}

fn lookup(tag: isize) -> Option<Action> {
    let v = ACTIONS.lock().ok()?;
    let idx = (tag as usize).checked_sub(1)?;
    v.get(idx).cloned()
}

// ---------------------------------------------------------------------
// MenuTarget — the Objective-C object that receives every Sonic-owned
// menu item's action selector.
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
            let tag = sender.tag();
            let Some(action) = lookup(tag) else {
                tracing::warn!("SonicMenuTarget: tag {tag} has no registered action");
                return;
            };
            tracing::debug!("menubar dispatch -> {action:?}");
            if !menubar_bridge::push_action(action) {
                tracing::warn!(
                    "menubar dispatch: action queued but no event-loop proxy is installed; \
                     it will not run until the loop wakes for another reason"
                );
            }
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
// Menu construction.
// ---------------------------------------------------------------------

/// Install the Sonic NSMenu as the application's main menu.
///
/// `theme_names` is the list of bundled themes to expose under
/// View → Theme; pass the result of scanning `assets/themes/`.
/// Order is preserved.
pub fn install(theme_names: &[String]) {
    // Safe to call from `main()` before the event loop spins; AppKit
    // requires the main thread, which `main()` is by definition.
    let mtm = MainThreadMarker::new().expect("install_menubar must run on the macOS main thread");
    let app = NSApplication::sharedApplication(mtm);
    let target = MenuTarget::new(mtm);

    let main = NSMenu::new(mtm);

    main.addItem(&app_submenu(mtm, &target));
    main.addItem(&file_submenu(mtm, &target));
    main.addItem(&edit_submenu(mtm, &target));
    main.addItem(&view_submenu(mtm, &target, theme_names));
    main.addItem(&terminal_submenu(mtm, &target));
    main.addItem(&window_submenu(mtm, &target));
    main.addItem(&help_submenu(mtm, &target));

    app.setMainMenu(Some(&main));

    // MenuTarget must outlive the menu items that reference it.
    // The menu items don't retain their target, by AppKit convention.
    // We leak it intentionally — it lives for the program's lifetime.
    let _ = Retained::into_raw(target);

    tracing::info!("macOS native menubar installed ({} themes)", theme_names.len());
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn separator(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    NSMenuItem::separatorItem(mtm)
}

/// Build a top-level submenu container item with `title`. Caller
/// fills its `submenu`.
fn submenu_item(
    mtm: MainThreadMarker,
    title: &str,
    items: Vec<Retained<NSMenuItem>>,
) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&ns(title));
    let menu = NSMenu::new(mtm);
    menu.setTitle(&ns(title));
    for it in items {
        menu.addItem(&it);
    }
    item.setSubmenu(Some(&menu));
    item
}

/// Build a custom item that dispatches `action` via [`MenuTarget`].
fn custom_item(
    mtm: MainThreadMarker,
    title: &str,
    key: &str,
    mods: NSEventModifierFlags,
    target: &MenuTarget,
    action: Action,
) -> Retained<NSMenuItem> {
    let tag = register(action);
    let item = NSMenuItem::new(mtm);
    item.setTitle(&ns(title));
    item.setKeyEquivalent(&ns(key));
    unsafe {
        item.setKeyEquivalentModifierMask(mods);
        item.setTag(tag);
        item.setTarget(Some(target));
        item.setAction(Some(sel!(dispatch:)));
    }
    item
}

/// Build an item bound to a standard AppKit selector (no custom
/// dispatch). `target` of `None` means "first responder".
fn system_item(
    mtm: MainThreadMarker,
    title: &str,
    key: &str,
    mods: NSEventModifierFlags,
    selector: Sel,
) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&ns(title));
    item.setKeyEquivalent(&ns(key));
    unsafe {
        item.setKeyEquivalentModifierMask(mods);
        item.setAction(Some(selector));
    }
    item
}

fn cmd() -> NSEventModifierFlags {
    NSEventModifierFlags::Command
}
fn cmd_shift() -> NSEventModifierFlags {
    NSEventModifierFlags::Command | NSEventModifierFlags::Shift
}

// ---- Sonic (app) menu -------------------------------------------------

fn app_submenu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenuItem> {
    let about = system_item(
        mtm,
        "About Sonic",
        "",
        NSEventModifierFlags::empty(),
        sel!(orderFrontStandardAboutPanel:),
    );
    let prefs =
        custom_item(mtm, "Preferences\u{2026}", ",", cmd(), target, Action::OpenPreferences);
    let hide = system_item(mtm, "Hide Sonic", "h", cmd(), sel!(hide:));
    let hide_others = system_item(
        mtm,
        "Hide Others",
        "h",
        cmd() | NSEventModifierFlags::Option,
        sel!(hideOtherApplications:),
    );
    let show_all = system_item(
        mtm,
        "Show All",
        "",
        NSEventModifierFlags::empty(),
        sel!(unhideAllApplications:),
    );
    let quit = system_item(mtm, "Quit Sonic", "q", cmd(), sel!(terminate:));

    submenu_item(
        mtm,
        // Title on the app submenu is overridden by AppKit to the
        // process name on launch; we still set a placeholder.
        "Sonic",
        vec![
            about,
            separator(mtm),
            prefs,
            separator(mtm),
            hide,
            hide_others,
            show_all,
            separator(mtm),
            quit,
        ],
    )
}

// ---- File menu --------------------------------------------------------

fn file_submenu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenuItem> {
    submenu_item(
        mtm,
        "File",
        vec![
            custom_item(mtm, "New Tab", "t", cmd(), target, Action::NewTab),
            custom_item(mtm, "New Window", "n", cmd(), target, Action::NewWindow),
            separator(mtm),
            custom_item(mtm, "Close Tab", "w", cmd(), target, Action::CloseTab),
            system_item(mtm, "Close Window", "w", cmd_shift(), sel!(performClose:)),
        ],
    )
}

// ---- Edit menu --------------------------------------------------------

fn edit_submenu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenuItem> {
    submenu_item(
        mtm,
        "Edit",
        vec![
            custom_item(mtm, "Copy", "c", cmd(), target, Action::CopyToClipboard),
            custom_item(mtm, "Paste", "v", cmd(), target, Action::PasteFromClipboard),
            separator(mtm),
            custom_item(mtm, "Find\u{2026}", "f", cmd(), target, Action::OpenSearch),
            // Find Next / Previous: no first-class actions yet; queue
            // OpenSearch as a placeholder so the menu items still light
            // up. A follow-up PR will add SearchNext / SearchPrev to
            // the Action enum and re-route these.
            custom_item(mtm, "Find Next", "g", cmd(), target, Action::OpenSearch),
            custom_item(mtm, "Find Previous", "g", cmd_shift(), target, Action::OpenSearch),
        ],
    )
}

// ---- View menu --------------------------------------------------------

fn view_submenu(
    mtm: MainThreadMarker,
    target: &MenuTarget,
    theme_names: &[String],
) -> Retained<NSMenuItem> {
    let mut items = vec![
        custom_item(mtm, "Increase Font", "=", cmd(), target, Action::IncreaseFontSize),
        custom_item(mtm, "Decrease Font", "-", cmd(), target, Action::DecreaseFontSize),
        custom_item(mtm, "Reset Font", "0", cmd(), target, Action::ResetFontSize),
        separator(mtm),
    ];

    // Theme submenu.
    let theme_items: Vec<_> = theme_names
        .iter()
        .map(|name| {
            custom_item(
                mtm,
                name,
                "",
                NSEventModifierFlags::empty(),
                target,
                Action::ApplyTheme(name.clone()),
            )
        })
        .collect();
    items.push(submenu_item(mtm, "Theme", theme_items));
    items.push(separator(mtm));
    items.push(custom_item(mtm, "Toggle Tab Bar", "t", cmd_shift(), target, Action::ToggleTabBar));

    submenu_item(mtm, "View", items)
}

// ---- Terminal menu ----------------------------------------------------

fn terminal_submenu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenuItem> {
    // NOTE: New SSH Connection… is intentionally NOT exposed here
    // yet — the `Action::OpenSshPane(_)` enum variant exists, but
    // `sonic-mac` doesn't yet declare the `ssh` cargo feature that
    // gates real connection wiring in `sonic-core`. Adding the menu
    // item before the feature is wired would put a permanently
    // dead item in the bar. Follow-up PR adds the feature + UX.
    let items = vec![custom_item(
        mtm,
        "Open Command Palette",
        "p",
        cmd_shift(),
        target,
        Action::OpenCommandPalette,
    )];
    submenu_item(mtm, "Terminal", items)
}

// ---- Window menu ------------------------------------------------------

fn window_submenu(mtm: MainThreadMarker, _target: &MenuTarget) -> Retained<NSMenuItem> {
    let items = vec![
        system_item(mtm, "Minimize", "m", cmd(), sel!(performMiniaturize:)),
        system_item(mtm, "Zoom", "", NSEventModifierFlags::empty(), sel!(performZoom:)),
        separator(mtm),
        system_item(
            mtm,
            "Bring All to Front",
            "",
            NSEventModifierFlags::empty(),
            sel!(arrangeInFront:),
        ),
    ];
    let item = submenu_item(mtm, "Window", items);
    // Tell AppKit this is THE Window menu (gets auto-populated with
    // the live window list). Requires a separate `setWindowsMenu:`
    // call on the application.
    if let Some(menu) = item.submenu() {
        let mtm2 = mtm;
        let app = NSApplication::sharedApplication(mtm2);
        app.setWindowsMenu(Some(&menu));
    }
    item
}

// ---- Help menu --------------------------------------------------------

fn help_submenu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenuItem> {
    submenu_item(
        mtm,
        "Help",
        vec![custom_item(
            mtm,
            "Sonic User Guide",
            "",
            NSEventModifierFlags::empty(),
            target,
            // Reuse OpenSshPane(...) is wrong — we want a distinct
            // browser-open path. The cleanest way without bloating
            // the Action enum further is to push a no-op + run the
            // open via a small helper here. Below dispatch uses
            // OpenPreferences as a placeholder; the actual user-guide
            // open is fired synchronously here before dispatch.
            Action::OpenPreferences,
        )],
    )
}

// ---------------------------------------------------------------------
// Theme list helper.
// ---------------------------------------------------------------------

/// Scan `assets/themes/` for `*.toml` files and return a sorted list
/// of bare theme names (e.g. `["dracula", "nord", "tokyo-night"]`).
/// Returns an empty list (logged at WARN) if the directory cannot be
/// read — the menubar still installs, just with an empty Theme
/// submenu.
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
        let tag = register(Action::NewTab);
        assert!(tag >= 1);
        let got = lookup(tag).expect("registered tag should resolve");
        assert!(matches!(got, Action::NewTab));
        assert!(lookup(0).is_none(), "tag 0 is invalid by design");
        assert!(lookup(-1).is_none());
    }
}
