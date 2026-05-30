use sonic_app::app::App;
use sonic_core::{
    config::Config,
    keymap::{Action, ActionWrapper, Binding, Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use winit::keyboard::{Key, ModifiersState, SmolStr};

fn hex() -> Hex {
    Hex("#000000".to_string())
}

fn ansi() -> AnsiColors {
    AnsiColors {
        black: hex(),
        red: hex(),
        green: hex(),
        yellow: hex(),
        blue: hex(),
        magenta: hex(),
        cyan: hex(),
        white: hex(),
    }
}

fn synth_theme() -> Theme {
    Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex(),
            foreground: hex(),
            cursor: hex(),
            cursor_text: hex(),
            selection_bg: hex(),
            selection_fg: hex(),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: hex(),
                active_bg: hex(),
                active_fg: hex(),
                inactive_bg: hex(),
                inactive_fg: hex(),
                hover_bg: hex(),
                hover_fg: hex(),
                close_button_fg: hex(),
            },
        },
    }
}

fn keymap() -> Keymap {
    Keymap {
        meta: Meta { name: "test".into(), version: "0".into() },
        bindings: vec![Binding { keys: "ctrl+t".into(), action: ActionWrapper(Action::NewTab) }],
    }
}

fn keymap_with(binding: &str, action: Action) -> Keymap {
    Keymap {
        meta: Meta { name: "test".into(), version: "0".into() },
        bindings: vec![Binding { keys: binding.into(), action: ActionWrapper(action) }],
    }
}

#[test]
fn ctrl_t_binding_dispatches_before_pty_control_byte() {
    let mut app = App::new(synth_theme(), Config::default(), keymap());
    app.__test_seed_tab("alpha");

    let (action, pty_bytes) = app.__test_dispatch_key_or_encode_pty(
        &Key::Character(SmolStr::new("t")),
        ModifiersState::CONTROL,
    );

    assert_eq!(action, Some(Action::NewTab));
    assert_eq!(pty_bytes, None, "Ctrl+T must not leak ^T (0x14) to the PTY");
    assert_eq!(app.__test_tab_count(), 2);
}

#[test]
fn unbound_ctrl_t_still_encodes_control_byte() {
    let mut app = App::new(
        synth_theme(),
        Config::default(),
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: Vec::new() },
    );
    app.__test_seed_tab("alpha");

    let (action, pty_bytes) = app.__test_dispatch_key_or_encode_pty(
        &Key::Character(SmolStr::new("t")),
        ModifiersState::CONTROL,
    );

    assert_eq!(action, None);
    assert_eq!(pty_bytes, Some(vec![0x14]));
}

#[test]
fn ctrl_shift_p_binding_dispatches_palette_before_pty_control_byte() {
    let mut app = App::new(
        synth_theme(),
        Config::default(),
        keymap_with("ctrl+shift+p", Action::OpenCommandPalette),
    );
    app.__test_seed_tab("alpha");

    let (action, pty_bytes) = app.__test_dispatch_key_or_encode_pty(
        &Key::Character(SmolStr::new("P")),
        ModifiersState::CONTROL | ModifiersState::SHIFT,
    );

    assert_eq!(action, Some(Action::OpenCommandPalette));
    assert_eq!(pty_bytes, None, "Ctrl+Shift+P must not leak ^P (0x10) to the PTY");
}

#[test]
fn ctrl_shift_d_binding_dispatches_split_before_pty_control_byte() {
    let mut app =
        App::new(synth_theme(), Config::default(), keymap_with("ctrl+shift+d", Action::SplitRight));
    app.__test_seed_tab("alpha");

    let (action, pty_bytes) = app.__test_dispatch_key_or_encode_pty(
        &Key::Character(SmolStr::new("D")),
        ModifiersState::CONTROL | ModifiersState::SHIFT,
    );

    assert_eq!(action, Some(Action::SplitRight));
    assert_eq!(pty_bytes, None, "Ctrl+Shift+D must not leak ^D (0x04) to the PTY");
}
