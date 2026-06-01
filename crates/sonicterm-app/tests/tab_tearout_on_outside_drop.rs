use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sonicterm_app::app::App;
use sonicterm_app::os_drag::{DragAck, OsDragSink, TabPayload};
use sonicterm_core::pty::PtyHandle;
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

struct AcceptingTearoutSink {
    calls: AtomicUsize,
}

impl AcceptingTearoutSink {
    fn new() -> Arc<Self> {
        Arc::new(Self { calls: AtomicUsize::new(0) })
    }
}

impl OsDragSink for AcceptingTearoutSink {
    fn begin_drag(&self, _payload: &TabPayload) -> DragAck {
        self.calls.fetch_add(1, Ordering::SeqCst);
        DragAck::Accepted
    }
}

#[test]
fn outside_drop_handoff_moves_source_tab_out_of_window() {
    let mut app = synth_app();
    let moved_pane = app.__test_seed_tab("source");
    if let Ok(shell) = std::env::var("COMSPEC") {
        if let Ok(pty) = PtyHandle::spawn(&shell, 80, 24) {
            assert!(app.__test_set_pane_pty(moved_pane, Some(pty)));
        }
    }
    let surviving_pane = app.__test_seed_tab("survivor");
    let sink = AcceptingTearoutSink::new();
    app.__test_set_os_drag_sink(sink.clone() as Arc<dyn OsDragSink>);

    let handed_off = app.__test_try_os_drag_handoff(0);

    assert!(handed_off, "outside drop should be consumed when tear-out sink accepts");
    assert_eq!(sink.calls.load(Ordering::SeqCst), 1);
    assert_eq!(app.__test_tab_count(), 1, "source tab should be removed after accepted tear-out");
    let panes = app.__test_pane_ids();
    assert!(!panes.contains(&moved_pane), "moved pane must leave the source window");
    assert!(panes.contains(&surviving_pane), "unrelated source-window pane must remain");
    assert!(
        app.__test_pane_pty_present(moved_pane).is_none(),
        "moved pane's PtyHandle must not remain in the origin window"
    );
}
