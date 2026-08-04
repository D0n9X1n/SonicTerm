//! Native macOS `NSMenu` for SonicTerm Terminal.
//!
//! Top-level submenus (in order): **SonicTerm / Shell / Edit / View / Help**.
//! Items dispatch to `sonicterm_cfg::keymap::Action`s via the
//! [`sonicterm_app::menubar_bridge`] queue; the winit loop drains and
//! routes through `App::run_action` — the same path used by keybindings.
//!
//! Help items that point to URLs are opened directly from the AppKit
//! main thread via `NSWorkspace::openURL:` so no new `Action` variant
//! is required.
//!
//! Shared blueprint + types live in [`sonicterm_app::menu`]; this file
//! is now the macOS-specific [`PlatformMenu`] implementation only.

#![cfg(target_os = "macos")]

use std::path::Path;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem, NSWorkspace};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString, NSURL};

use sonicterm_app::menu::{self, PlatformMenu, Sender};
use sonicterm_cfg::keymap::Action;

// Re-export shared blueprint types so external integration tests and
// call sites that referenced `menubar::Item` / `Binding` / `KeyMods`
// still compile.
pub use sonicterm_app::menu::{blueprint, Binding, Item, KeyMods, MenuBlueprint, Submenu};

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
    /// Reveal the SonicTerm log directory in Finder.
    ShowLogsInFinder,
    /// Run the aggressive "clear all rotated logs + crashes" pass and
    /// show an AppKit alert with the count and bytes freed.
    ClearOldLogs,
}

static ENTRIES: Mutex<Vec<MenuEntry>> = Mutex::new(Vec::new());

fn register(entry: MenuEntry) -> isize {
    // PANIC: lock poisoning indicates a prior panic while another thread held
    // the registry — process state is corrupt and continuing risks UB in the
    // menu callbacks. Crashing here is the safe option.
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
        // When: lookup holds no entry for tag, so the click names an item this
        // registry never assigned.
        tracing::warn!("SonicTermMenuTarget: tag {tag} has no registered entry");
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
        MenuEntry::ShowLogsInFinder => {
            let dir = sonicterm_logging::log_dir();
            // Use file:// URL so NSWorkspace opens the dir in Finder.
            let url = format!("file://{}", dir.display());
            open_url(&url);
            true
        }
        MenuEntry::ClearOldLogs => {
            // When: entry is ClearOldLogs, so removal runs here and the freed
            // count reaches the user through a spawned notification.
            let dir = sonicterm_logging::log_dir();
            let (n, bytes) = sonicterm_logging::clear_all_rotated(&dir);
            let mb = (bytes as f64) / (1024.0 * 1024.0);
            tracing::info!(files = n, mb, "menubar: cleared old logs");
            // Best-effort native notification: a banner via osascript
            // so we don't add a heavyweight NSAlert dependency.
            let body = format!("Cleared {n} files ({mb:.2} MB) from SonicTerm logs.");
            let script = format!(
                "display notification \"{}\" with title \"SonicTerm\"",
                body.replace('"', "")
            );
            let _ = std::process::Command::new("osascript").arg("-e").arg(&script).spawn();
            true
        }
    }
}

fn open_url(url: &str) {
    // Best-effort: invalid URLs are silently ignored (logged at WARN).
    let nsurl = NSURL::URLWithString(&NSString::from_str(url));
    if let Some(nsurl) = nsurl {
        // When: nsurl parsed, so the string named a real URL and it can be
        // handed to NSWorkspace from the main thread.
        let _ = MainThreadMarker::new()
            // PANIC: safe — every caller of `open_url` is dispatched from
            // AppKit menu actions, which AppKit guarantees fire on the main
            // thread. Calling from any other thread is a programmer bug
            // that would be caught immediately during dev — crash early.
            .expect("open_url must run on the macOS main thread (AppKit invariant)");
        let workspace = NSWorkspace::sharedWorkspace();
        workspace.openURL(&nsurl);
    } else {
        // When: nsurl is absent, so URLWithString rejected the string and the
        // click is dropped rather than opening something unintended.
        tracing::warn!("menubar: ignoring malformed URL {url:?}");
    }
}

// ---------------------------------------------------------------------
// MenuTarget — the Objective-C action receiver.
// ---------------------------------------------------------------------

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "SonicTermMenuTarget"]
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
        // SAFETY: `this` is a freshly allocated `MenuTarget` whose ivars are
        // set, and `init` is `NSObject`'s designated initializer, so it runs
        // exactly once on an object that has not been initialised yet.
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
        // PANIC: safe — the input strings come from the const menu blueprint
        // (see `Blueprint::system` callers), which is authored alongside this
        // match. Any new selector added to the blueprint must be added here
        // as well; mismatch is a dev-time bug, not a runtime risk.
        other => panic!("unknown system selector in menu blueprint: {other}"),
    }
}

fn build_item(mtm: MainThreadMarker, item: &Item, target: &MenuTarget) -> Retained<NSMenuItem> {
    if matches!(item.binding, Binding::Separator) {
        // When: matches reports a separator, which carries no title, key, or
        // action and is built by AppKit's own constructor.
        return NSMenuItem::separatorItem(mtm);
    }
    let nsi = NSMenuItem::new(mtm);
    nsi.setTitle(&ns(item.title));
    nsi.setKeyEquivalent(&ns(item.key));
    nsi.setKeyEquivalentModifierMask(flags(item.mods));
    match &item.binding {
        Binding::Action(a) => {
            // When: the binding is an Action, so the item carries a registry
            // tag and routes through the shared dispatch selector.
            let tag = register(MenuEntry::Act(a.clone()));
            // SAFETY: `nsi` is a live `NSMenuItem` this function just created,
            // and `target` outlives it — the `MenuTarget` is leaked for the
            // process lifetime in `MacMenu::install`.
            unsafe {
                nsi.setTag(tag);
                nsi.setTarget(Some(target));
                nsi.setAction(Some(sel!(dispatch:)));
            }
        }
        Binding::Url(url) => {
            // When: the binding is a Url, so the item carries a registry tag
            // whose entry opens that address on click.
            let tag = register(MenuEntry::Url((*url).to_string()));
            // SAFETY: `nsi` is a live `NSMenuItem` this function just created,
            // and `target` outlives it — the `MenuTarget` is leaked for the
            // process lifetime in `MacMenu::install`.
            unsafe {
                nsi.setTag(tag);
                nsi.setTarget(Some(target));
                nsi.setAction(Some(sel!(dispatch:)));
            }
        }
        Binding::System(name) => {
            // When: the binding is a System selector, so AppKit routes the
            // click through the responder chain with no target and no tag.

            // SAFETY: `ns_selector_from_str` returns a selector compiled into
            // this binary, and `nsi` is a live `NSMenuItem` this function just
            // created.
            unsafe {
                nsi.setAction(Some(ns_selector_from_str(name)));
            }
        }
        // PANIC: safe — `Binding::Separator` is intercepted by the caller
        // (see `MenuItem::separator()` branch in build_menu before this fn
        // is invoked); reaching it here would indicate a refactor missed
        // the caller-side dispatch. Structurally unreachable.
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

/// A standalone Window-menu item bound to a first-responder selector
/// (`performMiniaturize:`, `performZoom:`, `arrangeInFront:`). These have
/// **no explicit target** so AppKit routes them through the key window's
/// responder chain — exactly like a `Binding::System` item.
fn build_responder_item(
    mtm: MainThreadMarker,
    title: &str,
    selector: Sel,
    key: &str,
    mods: KeyMods,
) -> Retained<NSMenuItem> {
    let nsi = NSMenuItem::new(mtm);
    nsi.setTitle(&ns(title));
    nsi.setKeyEquivalent(&ns(key));
    nsi.setKeyEquivalentModifierMask(flags(mods));
    // SAFETY: `selector` is a compile-time selector constant and `nsi` is a
    // live `NSMenuItem` this function just created. No target is set, which is
    // what routes the click through the key window's responder chain.
    unsafe { nsi.setAction(Some(selector)) };
    nsi
}

/// Build the standard macOS **Window** menu and hand it to AppKit via
/// `setWindowsMenu:`. Once registered, AppKit auto-populates the menu with
/// every open `NSWindow` (each torn-out SonicTerm window included), keeps
/// the list live as windows open/close, checks the key window, and wires
/// the system ⌘` "cycle windows" key equivalent — matching Terminal.app /
/// WezTerm. We only author the static Minimize / Zoom / Bring-All-to-Front
/// items; the dynamic window list below the separator is AppKit's.
fn install_window_menu(mtm: MainThreadMarker, app: &NSApplication, main: &NSMenu) {
    let container = NSMenuItem::new(mtm);
    container.setTitle(&ns("Window"));
    let m = NSMenu::new(mtm);
    m.setTitle(&ns("Window"));
    m.addItem(&build_responder_item(mtm, "Minimize", sel!(performMiniaturize:), "m", KeyMods::Cmd));
    m.addItem(&build_responder_item(mtm, "Zoom", sel!(performZoom:), "", KeyMods::None));
    m.addItem(&NSMenuItem::separatorItem(mtm));
    m.addItem(&build_responder_item(
        mtm,
        "Bring All to Front",
        sel!(arrangeInFront:),
        "",
        KeyMods::None,
    ));
    container.setSubmenu(Some(&m));
    main.addItem(&container);
    // Registering the menu is what unlocks the auto window list + ⌘`
    // cycling; without this AppKit treats it as an ordinary submenu.
    app.setWindowsMenu(Some(&m));
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
    /// Build the macOS menu from the shared blueprint.
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
            // The standard macOS Window menu conventionally sits just
            // before Help. Insert it here so AppKit owns the live window
            // list (all torn-out windows) + ⌘` cycling.
            if sm.title == "Help" {
                install_window_menu(mtm, &app, &main);
            }
            let item = build_submenu(mtm, sm, &target);
            // Append the logging affordances to the Help submenu.
            if sm.title == "Help" {
                if let Some(menu) = item.submenu() {
                    menu.addItem(&NSMenuItem::separatorItem(mtm));
                    menu.addItem(&build_custom_item(
                        mtm,
                        "Show Logs in Finder",
                        MenuEntry::ShowLogsInFinder,
                        &target,
                    ));
                    menu.addItem(&build_custom_item(
                        mtm,
                        "Clear Old Logs",
                        MenuEntry::ClearOldLogs,
                        &target,
                    ));
                }
            }
            main.addItem(&item);
        }
        app.setMainMenu(Some(&main));

        // MenuTarget must outlive the menu items that reference it.
        // Leak intentionally — lives for the program's lifetime.
        let _ = Retained::into_raw(target);

        tracing::info!("macOS native menubar installed");
        Ok(())
    }
}

fn build_custom_item(
    mtm: MainThreadMarker,
    title: &str,
    entry: MenuEntry,
    target: &MenuTarget,
) -> Retained<NSMenuItem> {
    let nsi = NSMenuItem::new(mtm);
    nsi.setTitle(&ns(title));
    let tag = register(entry);
    // SAFETY: `nsi` is a live `NSMenuItem` this function just created, and
    // `target` outlives it — the `MenuTarget` is leaked for the process
    // lifetime in `MacMenu::install`.
    unsafe {
        nsi.setTag(tag);
        nsi.setTarget(Some(target));
        nsi.setAction(Some(sel!(dispatch:)));
    }
    nsi
}

/// Install the SonicTerm NSMenu as the application's main menu. The
/// `_theme_names` argument is accepted for backward compatibility with
/// existing call sites; the blueprint no longer surfaces themes in the
/// menubar.
pub fn install(_theme_names: &[String]) {
    if let Err(e) = MacMenu::new().install(Sender::new()) {
        tracing::error!("install_menubar: {e}");
    }
}

// ---------------------------------------------------------------------
// Theme list helper (kept for callers that still scan).
// ---------------------------------------------------------------------

/// List the `.toml` theme stems under `themes_dir`, sorted.
///
/// An unreadable directory yields an empty list rather than an error: a missing
/// themes directory is a normal state, not a startup failure.
pub fn scan_themes(themes_dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(themes_dir) else {
        // When: read_dir cannot open themes_dir, so no theme file is visible
        // and the caller receives an empty list.
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
                // When: the extension is not toml, so the entry is not a theme
                // file and contributes no name to the scan.
                None
            }
        })
        .collect();
    names.sort();
    names
}
