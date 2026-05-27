//! Native Windows menubar via the `muda` crate.
//!
//! The shared blueprint ([`sonic_app::menu::blueprint`]) is walked
//! and materialized into a `muda::Menu` attached to the main HWND.
//! Click events arrive on `muda::MenuEvent::receiver()`; the winit
//! event loop drains them once per frame (see `main.rs`).
//!
//! `Binding::System(...)` selectors are macOS-only Objective-C names
//! and are skipped here (logged WARN). `Binding::Url(...)` is opened
//! through `sonic_core::url_open`, which already applies the allow-
//! list / control-char denylist mandated by §4 of CLAUDE.md.

#![cfg(target_os = "windows")]

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result};
use muda::{
    accelerator::{Accelerator, Code, Modifiers},
    AboutMetadata, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu as MudaSubmenu,
};
use windows::Win32::Foundation::HWND;

use sonic_app::menu::{self, Binding, Item, KeyMods, PlatformMenu, Sender, Submenu};
use sonic_core::keymap::Action;

/// What activating a `muda` item should do once the event reaches the
/// winit loop. Mirrors `MenuEntry` on the macOS side.
#[derive(Debug, Clone)]
enum WinEntry {
    Act(Action),
    Url(String),
}

static REGISTRY: Mutex<Option<HashMap<MenuId, WinEntry>>> = Mutex::new(None);

fn registry_insert(id: MenuId, entry: WinEntry) {
    let mut guard = REGISTRY.lock().expect("muda registry poisoned");
    guard.get_or_insert_with(HashMap::new).insert(id, entry);
}

fn registry_lookup(id: &MenuId) -> Option<WinEntry> {
    let guard = REGISTRY.lock().ok()?;
    guard.as_ref()?.get(id).cloned()
}

/// The Windows platform menu. Owns the HWND it installs against.
pub struct WinMenu {
    hwnd: HWND,
}

impl WinMenu {
    pub fn new(hwnd: HWND) -> Self {
        Self { hwnd }
    }
}

fn modifiers(m: KeyMods) -> Modifiers {
    // `Cmd` maps to `Control` on Windows — the canonical Windows shortcut prefix.
    match m {
        KeyMods::None => Modifiers::empty(),
        KeyMods::Cmd => Modifiers::CONTROL,
        KeyMods::CmdShift => Modifiers::CONTROL | Modifiers::SHIFT,
        KeyMods::CmdOpt => Modifiers::CONTROL | Modifiers::ALT,
    }
}

/// Translate the blueprint's single-character `key` into a muda `Code`.
/// Returns `None` for unsupported characters; the caller then omits the
/// accelerator entirely (still renders the item, just without shortcut).
fn key_code(key: &str) -> Option<Code> {
    let c = key.chars().next()?;
    Some(match c.to_ascii_lowercase() {
        'a' => Code::KeyA,
        'b' => Code::KeyB,
        'c' => Code::KeyC,
        'd' => Code::KeyD,
        'e' => Code::KeyE,
        'f' => Code::KeyF,
        'g' => Code::KeyG,
        'h' => Code::KeyH,
        'i' => Code::KeyI,
        'j' => Code::KeyJ,
        'k' => Code::KeyK,
        'l' => Code::KeyL,
        'm' => Code::KeyM,
        'n' => Code::KeyN,
        'o' => Code::KeyO,
        'p' => Code::KeyP,
        'q' => Code::KeyQ,
        'r' => Code::KeyR,
        's' => Code::KeyS,
        't' => Code::KeyT,
        'u' => Code::KeyU,
        'v' => Code::KeyV,
        'w' => Code::KeyW,
        'x' => Code::KeyX,
        'y' => Code::KeyY,
        'z' => Code::KeyZ,
        '0' => Code::Digit0,
        '1' => Code::Digit1,
        '2' => Code::Digit2,
        '3' => Code::Digit3,
        '4' => Code::Digit4,
        '5' => Code::Digit5,
        '6' => Code::Digit6,
        '7' => Code::Digit7,
        '8' => Code::Digit8,
        '9' => Code::Digit9,
        ',' => Code::Comma,
        _ => return None,
    })
}

fn accelerator(item: &Item) -> Option<Accelerator> {
    let code = key_code(item.key)?;
    Some(Accelerator::new(Some(modifiers(item.mods)), code))
}

fn build_submenu(sm: &Submenu) -> Result<MudaSubmenu> {
    let muda_sm = MudaSubmenu::new(sm.title, true);
    for it in &sm.items {
        match &it.binding {
            Binding::Separator => {
                muda_sm
                    .append(&PredefinedMenuItem::separator())
                    .with_context(|| format!("muda separator in {}", sm.title))?;
            }
            Binding::Action(action) => {
                let mi = MenuItem::new(it.title, true, accelerator(it));
                registry_insert(mi.id().clone(), WinEntry::Act(action.clone()));
                muda_sm
                    .append(&mi)
                    .with_context(|| format!("muda action item {} → {}", sm.title, it.title))?;
            }
            Binding::Url(url) => {
                let mi = MenuItem::new(it.title, true, accelerator(it));
                registry_insert(mi.id().clone(), WinEntry::Url((*url).to_string()));
                muda_sm
                    .append(&mi)
                    .with_context(|| format!("muda url item {} → {}", sm.title, it.title))?;
            }
            Binding::System(sel) => {
                // macOS Objective-C selectors. A few have natural Windows
                // analogues via `PredefinedMenuItem`; the rest we skip.
                match *sel {
                    "orderFrontStandardAboutPanel:" => {
                        let about = PredefinedMenuItem::about(
                            Some("About Sonic"),
                            Some(AboutMetadata::default()),
                        );
                        muda_sm.append(&about).context("muda about")?;
                    }
                    "terminate:" => {
                        muda_sm
                            .append(&PredefinedMenuItem::quit(Some("Quit Sonic")))
                            .context("muda quit")?;
                    }
                    other => {
                        tracing::warn!(
                            "WinMenu: skipping macOS-only system selector {other:?} in {}",
                            sm.title
                        );
                    }
                }
            }
        }
    }
    Ok(muda_sm)
}

impl PlatformMenu for WinMenu {
    fn install(&self, sender: Sender) -> Result<()> {
        let menu = Menu::new();
        for sm in menu::blueprint() {
            let sub = build_submenu(&sm)?;
            menu.append(&sub).with_context(|| format!("attach submenu {}", sm.title))?;
        }

        // SAFETY: HWND is provided by winit and lives for the program;
        // muda's `init_for_hwnd` is the documented entry point.
        unsafe {
            menu.init_for_hwnd(self.hwnd.0 as isize).context("muda::Menu::init_for_hwnd")?;
        }

        // Spawn a tiny pump thread that reads muda's global event
        // receiver and forwards every click into the existing
        // menubar_bridge queue. This lets us reuse the same UserEvent
        // wake-up path the macOS impl uses, with zero changes to App.
        std::thread::Builder::new()
            .name("muda-menu-pump".into())
            .spawn(move || muda_pump(sender))
            .context("spawn muda pump thread")?;

        tracing::info!("Windows native menubar installed");
        Ok(())
    }
}

fn muda_pump(sender: Sender) {
    let rx = MenuEvent::receiver();
    while let Ok(ev) = rx.recv() {
        let Some(entry) = registry_lookup(&ev.id) else {
            tracing::warn!("muda: unknown menu id {:?}", ev.id);
            continue;
        };
        match entry {
            WinEntry::Act(action) => {
                tracing::debug!("muda dispatch -> {action:?}");
                sender.push(action);
            }
            WinEntry::Url(url) => {
                if let Err(e) = sonic_core::url_open::open(&url) {
                    tracing::warn!("muda url open failed: {e}");
                }
            }
        }
    }
}
