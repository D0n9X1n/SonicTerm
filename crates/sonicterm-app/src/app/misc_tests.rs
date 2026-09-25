use super::*;
use sonicterm_gpu::{
    core::{build_snapped_cell_x, emit_cell_bg_quads_for_row},
    row_quad_cache::{row_quad_hash_cells, CachedRowQuads, LineQuadCache},
};
use sonicterm_text::row_glyph_cache::row_hash_cells;

/// A broadcast receiver's DECSET 2004 mode, not the source mode, controls its clipboard guards.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_paste_keeps_mixed_bracketed_destinations() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::isolated,
    };
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::broadcast::BroadcastState;
    if isolated() {
        return;
    }
    let (mut app, windows) = input_test_windows();
    let (source_window, source) = windows[0];
    let (_, receiver) = windows[1];
    let (_, peer) = windows[2];
    app.pane_by_id(receiver).unwrap().parser.lock().advance(b"\x1b[?2004h");
    app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
    app.__test_set_memory_clipboard("paste");
    app.wait_for_input_queues();
    let submitted = PtySubmissions::start();
    let kind = app.kind_for(source_window);
    app.paste_clipboard_for_kind(kind);
    assert_eq!(
        submitted.take(),
        vec![
            (source, b"paste".to_vec()),
            (receiver, b"\x1b[200~paste\x1b[201~".to_vec()),
            (peer, b"paste".to_vec()),
        ]
    );
}

/// All four source/receiver guard combinations apply to clipboard text in main, child, Tab, and AllTabs routes.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_paste_clipboard_destination_matrix() {
    if crate::app::pty_test_support::isolated() {
        return;
    }
    assert_paste_destination_matrix(false);
}

/// File paths use each shell's quotes and guards without Enter; missing PTYs keep Unknown/POSIX without stopping peers.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_paste_paths_destination_matrix() {
    if crate::app::pty_test_support::isolated() {
        return;
    }
    assert_paste_destination_matrix(true);
}

#[cfg(any(windows, unix))]
fn assert_paste_destination_matrix(paths: bool) {
    use crate::app::mod_tests::{input_test_windows, PtySubmissions};
    use sonicterm_cfg::keymap::{BroadcastScope, Direction};
    use sonicterm_ui::broadcast::BroadcastState;
    use std::collections::BTreeSet;
    for scope in [BroadcastScope::Tab, BroadcastScope::AllTabs] {
        for source_index in 0..3 {
            let (mut app, windows) = input_test_windows();
            let (source_window, source) = windows[source_index];
            let (_, receiver) = windows[(source_index + 1) % 3];
            let (_, peer) = windows[(source_index + 2) % 3];
            let missing_window = app.__test_seed_child_window(&["no PTY"]);
            let missing = app.windows[&missing_window].tab_states[0].active_pane;
            if scope == BroadcastScope::Tab {
                // Move existing real panes, not their native processes, into the source's split tab.
                for (window, pane_id) in windows.into_iter().chain([(missing_window, missing)]) {
                    if pane_id == source {
                        continue;
                    }
                    let pane =
                        app.windows.get_mut(&window).unwrap().panes.remove(&pane_id).unwrap();
                    app.windows.get_mut(&window).unwrap().tab_states.clear();
                    let ws = app.windows.get_mut(&source_window).unwrap();
                    ws.panes.insert(pane_id, pane);
                    assert!(ws.tab_states[0].tree.split(source, Direction::Right, pane_id));
                }
            }
            app.broadcast = BroadcastState::On { scope, source_pane: source };
            app.frontmost_window = Some(missing_window);
            app.__test_enable_pty_write_log();
            app.__test_set_memory_clipboard("p'aste");
            let kind = app.kind_for(source_window);
            let submitted = PtySubmissions::start();
            for source_bracketed in [false, true] {
                for receiver_bracketed in [false, true] {
                    for (pane, enabled) in
                        [(source, source_bracketed), (receiver, receiver_bracketed)]
                    {
                        app.pane_by_id(pane).unwrap().parser.lock().advance(if enabled {
                            b"\x1b[?2004h"
                        } else {
                            b"\x1b[?2004l"
                        });
                    }
                    app.wait_for_input_queues();
                    if paths {
                        app.paste_file_paths_for_kind(kind, ["it's.txt".into()]);
                    } else {
                        app.paste_clipboard_for_kind(kind);
                    }
                    let expected: Vec<_> = std::iter::once(source)
                        .chain(BTreeSet::from([receiver, peer, missing]))
                        .map(|pane| {
                            let text = if !paths {
                                "p'aste"
                            } else if cfg!(windows) && pane != missing {
                                "\"it's.txt\""
                            } else {
                                "'it'\\''s.txt'"
                            };
                            let bracketed = (pane == source && source_bracketed)
                                || (pane == receiver && receiver_bracketed);
                            let bytes = if bracketed {
                                format!("\x1b[200~{text}\x1b[201~").into_bytes()
                            } else {
                                text.as_bytes().to_vec()
                            };
                            (pane, bytes)
                        })
                        .collect();
                    assert_eq!(app.__test_drain_pty_writes(), expected,
                        "paths={paths} scope={scope:?} source={source_index} guards={source_bracketed}/{receiver_bracketed}");
                    let accepted = submitted.take();
                    if paths {
                        assert!(
                            accepted.iter().all(
                                |(_, bytes)| !bytes.ends_with(b"\r") && !bytes.ends_with(b"\n")
                            ),
                            "file drops add no Enter"
                        );
                    }
                    assert_eq!(
                        accepted,
                        expected
                            .into_iter()
                            .filter(|(pane, _)| *pane != missing)
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(app.frontmost_window, Some(missing_window));
                }
            }
        }
    }
}

/// A cmd-refused source or receiver does not block a later PowerShell peer, across every guard combination.
#[cfg(windows)]
#[test]
fn real_pty_paste_cmd_refusal_keeps_other_destinations() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::{isolated, phase, record_process},
    };
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::broadcast::BroadcastState;
    if isolated() {
        return;
    }
    let (mut app, windows) = input_test_windows();
    let (source_window, source) = windows[0];
    let (_, receiver) = windows[1];
    let peer_window = app.__test_seed_child_window(&["PowerShell", "missing"]);
    let peer = app.windows[&peer_window].tab_states[0].active_pane;
    phase(peer, "spawn-begin");
    let pty = PtyHandle::spawn_with_args(
        "powershell.exe",
        &["-NoLogo".into(), "-NoProfile".into()],
        80,
        24,
    )
    .expect("PowerShell input PTY");
    record_process(pty.pid().unwrap(), true);
    app.windows.get_mut(&peer_window).unwrap().panes.get_mut(&peer).unwrap().pty = Some(pty);
    phase(peer, "spawn-end");
    app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
    let kind = app.kind_for(source_window);
    let submitted = PtySubmissions::start();
    for source_bracketed in [false, true] {
        for receiver_bracketed in [false, true] {
            for (pane, enabled) in [(source, source_bracketed), (receiver, receiver_bracketed)] {
                app.pane_by_id(pane).unwrap().parser.lock().advance(if enabled {
                    b"\x1b[?2004h"
                } else {
                    b"\x1b[?2004l"
                });
            }
            app.wait_for_input_queues();
            app.paste_file_paths_for_kind(kind, ["100%.txt".into()]);
            assert_eq!(submitted.take(), vec![(peer, b"'100%.txt'".to_vec())]);
        }
    }
}

/// The encoding ledger isolates guard-sized overflow without sending cap-sized writes to a native shell.
#[test]
fn paste_oversized_destination_does_not_stop_peers() {
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::broadcast::BroadcastState;
    use std::collections::BTreeSet;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let source = app.__test_seed_tab("source");
    let source_window = app.main_window_id.unwrap();
    let receiver_window = app.__test_seed_child_window(&["receiver", "peer"]);
    let receiver = app.windows[&receiver_window].tab_states[0].active_pane;
    let peer = app.windows[&receiver_window].tab_states[1].active_pane;
    let payload = "x".repeat(sonicterm_io::pty::MAX_PTY_INPUT_MESSAGE_BYTES);
    app.__test_set_memory_clipboard(&payload);
    app.__test_enable_pty_write_log();
    app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
    let kind = app.kind_for(source_window);
    for source_bracketed in [false, true] {
        for receiver_bracketed in [false, true] {
            for (pane, enabled) in [(source, source_bracketed), (receiver, receiver_bracketed)] {
                app.pane_by_id(pane).unwrap().parser.lock().advance(if enabled {
                    b"\x1b[?2004h"
                } else {
                    b"\x1b[?2004l"
                });
            }
            app.paste_clipboard_for_kind(kind);
            let writes = app.__test_drain_pty_writes();
            let mut expected = BTreeSet::from([peer]);
            if !source_bracketed {
                expected.insert(source);
            }
            if !receiver_bracketed {
                expected.insert(receiver);
            }
            assert_eq!(writes.iter().map(|(pane, _)| *pane).collect::<BTreeSet<_>>(), expected);
            assert!(writes.iter().all(|(_, bytes)| bytes == payload.as_bytes()));
        }
    }
}

/// Windows drops preserve native units: one unpaired surrogate refuses the whole list, while valid lists add no Enter.
#[cfg(windows)]
#[test]
fn real_pty_windows_path_drop_is_atomic_and_never_adds_enter() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::{isolated, phase, record_process},
    };
    use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
    if isolated() {
        return;
    }
    let (mut app, windows) = input_test_windows();
    let powershell_window = app.__test_seed_child_window(&["PowerShell"]);
    let powershell_pane = app.windows[&powershell_window].tab_states[0].active_pane;
    phase(powershell_pane, "spawn-begin");
    let pty = PtyHandle::spawn_with_args(
        "powershell.exe",
        &["-NoLogo".into(), "-NoProfile".into()],
        80,
        24,
    )
    .expect("PowerShell drop PTY");
    record_process(pty.pid().unwrap(), true);
    app.windows.get_mut(&powershell_window).unwrap().panes.get_mut(&powershell_pane).unwrap().pty =
        Some(pty);
    phase(powershell_pane, "spawn-end");
    let valid = PathBuf::from(r"C:\private\first file.txt");
    let second = PathBuf::from(r"C:\private\it's.txt");
    let invalid =
        PathBuf::from(OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0xd800]));
    let submitted = PtySubmissions::start();
    for (window, pane) in windows.into_iter().chain([(powershell_window, powershell_pane)]) {
        for bracketed in [false, true] {
            for state in app.windows.values_mut() {
                state.notification = None;
            }
            app.frontmost_window =
                Some(if window == windows[0].0 { windows[1].0 } else { windows[0].0 });
            app.pane_by_id(pane).unwrap().parser.lock().advance(if bracketed {
                b"\x1b[?2004h"
            } else {
                b"\x1b[?2004l"
            });
            app.wait_for_input_queues();
            app.paste_file_paths_in_window(window, vec![valid.clone(), invalid.clone()]);
            assert!(
                submitted.take().is_empty(),
                "a valid prefix must never escape whole-list validation"
            );
            let notice = app.windows[&window].notification.as_ref().unwrap();
            assert_eq!(
                notice.message,
                "Paste refused for 1 of 1 destinations: NonUnicodePath (destinations: 1)"
            );
            assert_eq!(notice.level, sonicterm_ui::overlays::NotificationLevel::Warning);
            assert_eq!(
                app.windows.values().filter(|state| state.notification.is_some()).count(),
                1
            );
            assert!(!notice.message.contains("private"));
            assert!(!notice.message.contains("first file.txt"));
            assert!(!notice.message.contains('\u{fffd}'));
            app.windows.get_mut(&window).unwrap().notification = None;
            app.wait_for_input_queues();
            app.paste_file_paths_in_window(window, vec![valid.clone(), second.clone()]);
            let paths = if pane == powershell_pane {
                "'C:\\private\\first file.txt' 'C:\\private\\it''s.txt'"
            } else {
                "\"C:\\private\\first file.txt\" \"C:\\private\\it's.txt\""
            };
            let expected = if bracketed {
                format!("\x1b[200~{paths}\x1b[201~").into_bytes()
            } else {
                paths.as_bytes().to_vec()
            };
            let writes = submitted.take();
            assert_eq!(writes, vec![(pane, expected)]);
            assert!(writes
                .iter()
                .all(|(_, bytes)| !bytes.ends_with(b"\r") && !bytes.ends_with(b"\n")));
            assert!(app.windows.values().all(|state| state.notification.is_none()));
        }
    }
}

/// A standalone cmd percent path produces one counted warning on its source, without leaking the filename.
#[cfg(windows)]
#[test]
fn real_pty_paste_notice_counts_cmd_refusal() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::isolated,
    };
    if isolated() {
        return;
    }
    let (mut app, windows) = input_test_windows();
    let (source_window, _) = windows[1];
    app.frontmost_window = Some(windows[0].0);
    app.wait_for_input_queues();
    let submitted = PtySubmissions::start();
    app.paste_file_paths_in_window(source_window, vec![r"C:\tmp\100%.txt".into()]);
    assert!(submitted.take().is_empty());
    let notice = app.windows[&source_window].notification.as_ref().unwrap();
    assert_eq!(
        notice.message,
        "Paste refused for 1 of 1 destinations: CmdUnsafeCharacter (destinations: 1)"
    );
    assert_eq!(notice.level, sonicterm_ui::overlays::NotificationLevel::Warning);
    assert_eq!(app.windows.values().filter(|window| window.notification.is_some()).count(), 1);
    assert!(!notice.message.contains("100%"));
    assert!(!notice.message.contains(r"C:\tmp"));
}

/// Partial size refusal reports the refused and total counts while unbracketed peers still get the complete bytes.
#[test]
fn paste_notice_counts_partial_too_large() {
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::broadcast::BroadcastState;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main = app.__test_seed_tab("main");
    let source_window = app.__test_seed_child_window(&["source", "guarded"]);
    let source = app.windows[&source_window].tab_states
        [app.windows[&source_window].tabs.active_index()]
    .active_pane;
    let guarded = app.windows[&source_window]
        .tab_states
        .iter()
        .find(|tab| tab.active_pane != source)
        .unwrap()
        .active_pane;
    app.pane_by_id(guarded).unwrap().parser.lock().advance(b"\x1b[?2004h");
    app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
    let text = "x".repeat(MAX_PTY_INPUT_MESSAGE_BYTES);
    app.__test_set_memory_clipboard(&text);
    app.__test_enable_pty_write_log();
    app.paste_clipboard_for_kind(FrontmostKind::Child(source_window));
    let writes = app.__test_drain_pty_writes();
    assert_eq!(writes.iter().map(|(pane, _)| *pane).collect::<Vec<_>>(), [source, main]);
    assert!(writes.iter().all(|(_, bytes)| bytes == text.as_bytes()));
    let notice = app.windows[&source_window].notification.as_ref().unwrap();
    assert_eq!(notice.message, format!(
        "Paste refused for 1 of 3 destinations: TooLarge (destinations: 1, needed: {} bytes, maximum: {MAX_PTY_INPUT_MESSAGE_BYTES} bytes)",
        MAX_PTY_INPUT_MESSAGE_BYTES + 12));
    assert_eq!(notice.level, sonicterm_ui::overlays::NotificationLevel::Warning);
    assert_eq!(app.windows.values().filter(|window| window.notification.is_some()).count(), 1);
    assert!(!notice.message.contains("xxxx"));
}

/// All refused destinations produce one source-owned notice that contains metadata, never the rejected paths.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_paste_refusal_notice_is_source_owned() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::isolated,
    };
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::broadcast::BroadcastState;
    if isolated() {
        return;
    }
    let (mut app, windows) = input_test_windows();
    let submitted = PtySubmissions::start();
    for (source_window, source) in windows {
        for state in app.windows.values_mut() {
            state.notification = None;
        }
        app.frontmost_window = Some(windows[2].0);
        app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
        app.wait_for_input_queues();
        app.paste_file_paths_in_window(source_window, vec!["private\nfile.txt".into()]);
        assert!(submitted.take().is_empty());
        let notice =
            app.windows[&source_window].notification.as_ref().expect("source refusal notice");
        assert_eq!(
            notice.message,
            "Paste refused for 3 of 3 destinations: ControlCharacter (destinations: 3)"
        );
        assert_eq!(notice.level, sonicterm_ui::overlays::NotificationLevel::Warning);
        assert_eq!(app.windows.values().filter(|state| state.notification.is_some()).count(), 1);
        assert!(!notice.message.contains("private"));
        assert!(!notice.message.contains("file.txt"));
    }
}

/// A single notice groups shell refusals and exact over-cap sizes, including guard-dependent sizes on PTY-less peers.
#[cfg(windows)]
#[test]
fn real_pty_paste_refusal_notice_groups_kinds_counts_and_sizes() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::isolated,
    };
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::broadcast::BroadcastState;
    if isolated() {
        return;
    }
    let (mut app, windows) = input_test_windows();
    let (source_window, source) = windows[1];
    let missing_window = app.__test_seed_child_window(&["plain", "guarded", "guarded again"]);
    for tab in &app.windows[&missing_window].tab_states[1..] {
        app.pane_by_id(tab.active_pane).unwrap().parser.lock().advance(b"\x1b[?2004h");
    }
    app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
    app.frontmost_window = Some(windows[0].0);
    app.wait_for_input_queues();
    let submitted = PtySubmissions::start();
    let path = format!("%{}", "x".repeat(MAX_PTY_INPUT_MESSAGE_BYTES));
    app.paste_file_paths_in_window(source_window, vec![path.into()]);
    assert!(submitted.take().is_empty());
    let notice = app.windows[&source_window].notification.as_ref().expect("mixed refusal notice");
    assert_eq!(notice.message, format!(
        "Paste refused for 6 of 6 destinations: CmdUnsafeCharacter (destinations: 3); TooLarge (destinations: 1, needed: {} bytes, maximum: {MAX_PTY_INPUT_MESSAGE_BYTES} bytes); TooLarge (destinations: 2, needed: {} bytes, maximum: {MAX_PTY_INPUT_MESSAGE_BYTES} bytes)",
        MAX_PTY_INPUT_MESSAGE_BYTES + 3, MAX_PTY_INPUT_MESSAGE_BYTES + 15));
    assert_eq!(app.windows.values().filter(|state| state.notification.is_some()).count(), 1);
}

/// Oversized clipboard text uses the same count-and-size notice, and a successful paste leaves an existing notice alone.
#[test]
fn paste_refusal_notice_handles_clipboard_and_success() {
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::broadcast::BroadcastState;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let source = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["peer"]);
    app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
    app.__test_set_memory_clipboard(&"x".repeat(MAX_PTY_INPUT_MESSAGE_BYTES + 1));
    app.paste_clipboard_for_kind(FrontmostKind::Main);
    let message = app.main().unwrap().notification.as_ref().unwrap().message.clone();
    assert_eq!(message, format!(
        "Paste refused for 2 of 2 destinations: TooLarge (destinations: 2, needed: {} bytes, maximum: {MAX_PTY_INPUT_MESSAGE_BYTES} bytes)",
        MAX_PTY_INPUT_MESSAGE_BYTES + 1));
    assert!(app.windows[&child].notification.is_none());
    app.__test_set_memory_clipboard("ok");
    app.paste_clipboard_for_kind(FrontmostKind::Main);
    assert_eq!(app.main().unwrap().notification.as_ref().unwrap().message, message);
}

/// The platform-neutral collector preserves order, clears each turn, and refuses an entire invalid path list.
#[test]
fn winit_drop_collection_keeps_window_batches_atomic() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main_pane = app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    let child_pane = app.windows[&child].tab_states[0].active_pane;
    app.__test_enable_pty_write_log();
    for window in [main, child] {
        app.collect_winit_file_drop(window, "a".into());
        app.collect_winit_file_drop(window, "b".into());
    }
    assert!(app.__test_drain_pty_writes().is_empty());
    app.drain_winit_file_drops();
    let mut writes = app.__test_drain_pty_writes();
    writes.sort_by_key(|(pane, _)| *pane);
    assert_eq!(writes, [(main_pane, b"'a' 'b'".to_vec()), (child_pane, b"'a' 'b'".to_vec())]);
    for window in [main, child] {
        app.collect_winit_file_drop(window, "a".into());
        app.collect_winit_file_drop(window, "bad\npath".into());
    }
    app.drain_winit_file_drops();
    assert!(app.__test_drain_pty_writes().is_empty());
    assert!(app.pending_winit_file_drops.is_empty());
    app.drain_winit_file_drops();
    assert!(app.__test_drain_pty_writes().is_empty());
}

/// Collection checks arrival admission and drain checks live ownership, so delayed drops cannot revive READONLY or closed targets.
#[test]
fn winit_drop_collection_consumes_readonly_and_closed_sources() {
    use sonicterm_ui::copy_mode::CopyModeState;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_enable_pty_write_log();
    app.windows.get_mut(&main).unwrap().copy_mode = Some(CopyModeState::read_only_at((0, 0)));
    app.collect_winit_file_drop(main, "arrival-readonly".into());
    app.windows.get_mut(&main).unwrap().copy_mode = None;
    app.drain_winit_file_drops();
    assert!(app.__test_drain_pty_writes().is_empty());
    app.collect_winit_file_drop(main, "drain-readonly".into());
    app.windows.get_mut(&main).unwrap().copy_mode = Some(CopyModeState::read_only_at((0, 0)));
    app.collect_winit_file_drop(child, "closed-child".into());
    assert!(app.close_child_window(child));
    app.frontmost_window = Some(main);
    app.drain_winit_file_drops();
    assert!(app.__test_drain_pty_writes().is_empty());
    assert!(app.pending_winit_file_drops.is_empty());
    assert!(app.main().unwrap().notification.is_none());
}

#[test]
fn native_file_drop_keeps_destination_through_focus_changes_and_closure() {
    // A captured native destination must not paste into a later frontmost window or main fallback.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_enable_pty_write_log();
    for (owner, other) in [(main, child), (child, main)] {
        app.frontmost_window = Some(other);
        let pane_id = app.windows[&owner].tab_states[0].active_pane;
        app.paste_file_paths_in_window(owner, vec![std::path::PathBuf::from("native drop.txt")]);
        assert_eq!(app.__test_drain_pty_writes(), vec![(pane_id, b"'native drop.txt'".to_vec())]);
        assert_eq!(app.frontmost_window, Some(other));
    }
    assert!(app.close_child_window(child));
    app.frontmost_window = Some(main);
    app.paste_file_paths_in_window(child, vec![std::path::PathBuf::from("must-not-type")]);
    assert!(app.__test_drain_pty_writes().is_empty());
}

/// New-window requests retain their initiating dimensions instead of choosing a fixed destination at drain time.
#[test]
fn new_window_constructor_uses_requested_dimensions() {
    let source = include_str!("misc.rs");
    let start = source.find("pub(super) fn create_new_terminal_window(").unwrap();
    let constructor = &source[start..];
    assert!(constructor.contains(".with_inner_size(request.inner_size)"));
    assert!(!constructor.contains("LogicalSize::new(800.0, 500.0)"));
}

/// Prompt navigation must move cached colored rows without relying on later PTY output or dirty invalidation.
#[test]
fn prompt_navigation_reprojects_overlapping_colored_history_without_dirty_rows() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("history");
    let parser = app.main().unwrap().panes[&pane_id].parser.clone();
    {
        let mut guard = parser.lock();
        *guard = Parser::new(Grid::new(4, 3));
        let mut input = Vec::new();
        for row in 0..15 {
            input.extend_from_slice(b"\x1b]133;A\x07\x1b[48;2;120;40;80m");
            input.extend_from_slice(format!("r{row:02}\x1b[0m\r\n").as_bytes());
        }
        guard.advance(&input);
        guard.grid_mut().clear_dirty();
        assert!(guard.grid().scrollback_len() > 10);
        assert_eq!(guard.grid().dirty_count(), 0);
    }
    app.main_mut().unwrap().panes.get_mut(&pane_id).unwrap().viewport_top_abs = Some(10);
    let mut cache = LineQuadCache::new();
    cache.resize(6);
    let theme = Theme::default();
    let snapped = build_snapped_cell_x(0.0, 10.0, 4);
    let hash_at = |top, slot| {
        let guard = parser.lock();
        let row = guard.grid().row_at_abs(10).unwrap();
        row_quad_hash_cells(top, slot, row.iter(), 1, 10.0, 20.0, 0.0, 0.0, 40.0, 60.0, None)
    };
    let glyph_hash_at = |top, slot| {
        let guard = parser.lock();
        let row = guard.grid().row_at_abs(10).unwrap();
        row_hash_cells(top, slot, row.iter(), 1, 10.0, 20.0, 1.0, 0.0, 0.0, 40.0, 60.0, None)
    };
    let project = |top, slot| {
        let guard = parser.lock();
        let mut quads = Vec::new();
        emit_cell_bg_quads_for_row(
            guard.grid(),
            top,
            &theme,
            0.0,
            0.0,
            10.0,
            20.0,
            40.0,
            60.0,
            4,
            slot,
            &mut quads,
            &snapped,
        );
        quads
    };
    let first_key = hash_at(10, 0);
    let first_glyph_key = glyph_hash_at(10, 0);
    let first = project(10, 0);
    assert_eq!(first.len(), 1, "non-default background must emit visible geometry");
    cache.insert(pane_id, 10, first_key, CachedRowQuads { quads: first.clone() });
    cache.insert(pane_id + 1, 10, first_key, CachedRowQuads { quads: first.clone() });
    let revision = parser.lock().grid().revision();

    app.scroll_to_prompt(false);

    let top = app.main().unwrap().panes[&pane_id].viewport_top_abs.unwrap();
    assert_eq!(top, 9);
    assert_eq!(parser.lock().grid().revision(), revision);
    assert_eq!(parser.lock().grid().dirty_count(), 0);
    let shifted_key = hash_at(top, 1);
    let expected = project(top, 1);
    let replayed = cache
        .get(pane_id, 10, shifted_key)
        .map(|cached| cached.quads.clone())
        .unwrap_or_else(|| expected.clone());
    assert_ne!(glyph_hash_at(top, 1), first_glyph_key, "glyphs already track their viewport slot");
    assert_ne!(expected[0].rect, first[0].rect);
    assert_eq!(replayed[0].rect, expected[0].rect, "background must follow the same moved row");
    assert!(cache.get(pane_id + 1, 10, first_key).is_some());

    app.scroll_to_prompt(true);

    assert_eq!(app.main().unwrap().panes[&pane_id].viewport_top_abs, Some(10));
    assert_eq!(hash_at(10, 0), first_key);
    assert_eq!(parser.lock().grid().dirty_count(), 0);
}
