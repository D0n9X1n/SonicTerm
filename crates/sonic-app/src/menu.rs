//! Cross-platform menubar abstraction.
//!
//! The menubar is described once as a pure-data blueprint — a list of
//! [`Submenu`]s containing [`Item`]s. Each platform implementation
//! ([`PlatformMenu`]) walks the blueprint and materializes the native
//! widget tree (NSMenu on macOS, `muda::Menu` on Windows). All click
//! dispatch flows through [`Sender`], which is a thin wrapper around
//! the existing static [`crate::menubar_bridge`] queue + wake-up
//! proxy. This keeps the platform code free of static globals and
//! mockable in tests.
//!
//! The blueprint itself ([`blueprint`]) is the single source of truth
//! for menu *shape*. Platform implementations may decline to render
//! certain bindings (e.g. macOS `System` selectors are no-ops on
//! Windows) but the structure — titles, ordering, separators,
//! accelerators — is identical across platforms so docs, tests, and
//! user muscle memory agree.

use sonic_core::keymap::Action;

use crate::menubar_bridge;

/// Modifier-key shorthand for accelerators. Platform code translates
/// to `NSEventModifierFlags` (macOS) or `muda::accelerator::Modifiers`
/// (Windows). `Cmd` maps to `Control` on Windows by convention.
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
    /// Queue an [`Action`] via the menubar bridge.
    Action(Action),
    /// Open a URL via the platform's default handler.
    Url(&'static str),
    /// Bound to a standard platform selector / system command. The
    /// string is interpreted by each platform impl; unknown selectors
    /// on a given platform are skipped with a WARN log.
    System(&'static str),
    Separator,
}

/// A single leaf item in the blueprint.
#[derive(Debug, Clone)]
pub struct Item {
    pub title: &'static str,
    pub key: &'static str,
    pub mods: KeyMods,
    pub binding: Binding,
}

/// A top-level submenu in the blueprint.
#[derive(Debug, Clone)]
pub struct Submenu {
    pub title: &'static str,
    pub items: Vec<Item>,
}

/// Alias for the whole-menubar blueprint.
pub type MenuBlueprint = Vec<Submenu>;

/// Thin sender wrapper around the [`menubar_bridge`] static queue.
/// Cloneable + `Send` + `Sync` so platform menu implementations can
/// keep one per item callback without juggling lifetimes.
#[derive(Debug, Clone, Default)]
pub struct Sender;

impl Sender {
    pub fn new() -> Self {
        Self
    }

    /// Queue `action` for the next event-loop drain and wake the loop.
    /// Returns `true` if the wake-up was posted.
    pub fn push(&self, action: Action) -> bool {
        menubar_bridge::push_action(action)
    }
}

/// A platform menubar implementation. The shared loop hands in a
/// [`Sender`] and the impl wires every clickable item to push through
/// it. `install` may capture `&self` state (e.g. an HWND on Windows)
/// and is expected to run on whatever thread the platform requires —
/// callers always invoke from the windowing thread.
pub trait PlatformMenu {
    fn install(&self, sender: Sender) -> anyhow::Result<()>;
}

/// The canonical menubar blueprint. Pure: no platform calls; safe in
/// tests. Top-level order is **Sonic / Shell / Edit / View / Help**.
/// macOS prepends the system "Apple" menu automatically.
pub fn blueprint() -> MenuBlueprint {
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
                    title: "Edit sonic.toml…",
                    key: ",",
                    mods: Cmd,
                    binding: Action(sonic_core::keymap::Action::EditConfigFile),
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
                    title: "Close",
                    key: "w",
                    mods: Cmd,
                    binding: Action(sonic_core::keymap::Action::CloseActivePaneOrTab),
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

// Unit tests live in `tests/menu.rs`.
