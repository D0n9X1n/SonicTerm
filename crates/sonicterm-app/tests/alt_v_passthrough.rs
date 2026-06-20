#![cfg(target_os = "windows")]

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::Config,
    keymap::{Action, ActionWrapper, Binding, Keymap, Meta},
    theme::Theme,
};
use winit::keyboard::{Key, ModifiersState};

#[test]
fn alt_v_paste_binding_falls_through_to_pty_for_terminal_apps() {
    let keymap = Keymap {
        meta: Meta { name: "old-windows-default".into(), version: "1.0".into() },
        bindings: vec![Binding {
            keys: "alt+v".into(),
            action: ActionWrapper(Action::PasteFromClipboard),
        }],
    };
    let mut app = App::new(Theme::default(), Config::default(), keymap);

    let (action, bytes) =
        app.__test_dispatch_key_or_encode_pty(&Key::Character("v".into()), ModifiersState::ALT);

    assert_eq!(action, None);
    assert_eq!(bytes, Some(b"\x1bv".to_vec()));
}
