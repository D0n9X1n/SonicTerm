use sonicterm_app::app::App;
use sonicterm_app::os_drag::TabPayload;
use sonicterm_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};

fn synth_theme() -> Theme {
    let hex = || Hex("#000000".to_string());
    let ansi = || AnsiColors {
        black: hex(),
        red: hex(),
        green: hex(),
        yellow: hex(),
        blue: hex(),
        magenta: hex(),
        cyan: hex(),
        white: hex(),
    };
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

fn synth_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: Vec::new() };
    App::new(synth_theme(), Config::default(), keymap)
}

fn synth_payload(title: &str) -> TabPayload {
    TabPayload {
        pty_pid: 4242,
        tab_title: title.to_string(),
        scrollback_b64: TabPayload::encode_scrollback(b"prior output\n"),
        cwd: "/tmp".to_string(),
        cmd: "/bin/zsh".to_string(),
        env: vec![("TERM".into(), "xterm-256color".into())],
    }
}

#[test]
fn os_drag_payload_received_before_main_exists_is_queued_and_drained() {
    let mut app = synth_app();
    assert_eq!(app.__test_main_window_id(), None, "new App starts before do_resumed");

    let payload = synth_payload("incoming before resumed");
    let idx = app.new_tab_from_payload(&payload);

    assert_eq!(idx, 0, "pre-main payload reports the safe fallback index");
    assert_eq!(app.__test_pending_os_drag_payload_count(), 1, "payload was queued, not dropped");
    assert_eq!(app.__test_tab_count(), 0, "no main tabs exist before main WindowState is created");

    app.__test_synthetic_main();
    app.__test_seed_tab("shell");
    app.__test_drain_pending_os_drag_payloads();

    assert_eq!(app.__test_pending_os_drag_payload_count(), 0, "queued payload was consumed");
    assert_eq!(app.__test_tab_count(), 2, "queued OS-drag payload created a destination tab");
}
