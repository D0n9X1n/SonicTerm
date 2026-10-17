use sonicterm_cfg::keymap::{BroadcastScope, Direction, ScrollAction};
use sonicterm_logging::process_memory::ProcessMemory;
use sonicterm_logging::process_report::ChildProcess;

use super::*;

/// MiB as bytes, so fixture figures read in the report's own unit.
const MIB: usize = 1024 * 1024;

/// A retention whose seams are distinct whole MiB values.
fn retention(history_mib: usize) -> PaneRetention {
    PaneRetention {
        grid_visible: ResourceAmount { bytes: MIB / 2, items: 1 },
        grid_history: ResourceAmount { bytes: history_mib * MIB, items: 1 },
        parser: ResourceAmount { bytes: MIB / 4, items: 1 },
        ..PaneRetention::default()
    }
}

/// One visible renderer holding a 16 MiB glyph atlas.
fn renderer(label: &str) -> RendererSummary {
    RendererSummary {
        label: label.to_string(),
        role: "visible",
        glyph_atlas: ResourceAmount { bytes: 16 * MIB, items: 9_812 },
        image_atlas: ResourceAmount::default(),
        row_glyph_cache: ResourceAmount { bytes: 3 * MIB, items: 1_204 },
        row_quad_cache: ResourceAmount { bytes: MIB, items: 1_204 },
        software_frame: ResourceAmount::default(),
        vertex_scratch: ResourceAmount { bytes: MIB / 2, items: 1 },
        row_ink: ResourceAmount::default(),
        frame_scratch: ResourceAmount::default(),
        chrome_cache: ResourceAmount::default(),
        glyph_atlas_facts: sonicterm_gpu::core::GlyphAtlasFacts {
            dim: 2048,
            packed_pixels: 0,
            growths: 0,
            evictions: 0,
            fit: "1024".to_string(),
            max_tile: [0, 0],
        },
        trimmed: false,
        gpu_released_requested_bytes: 0,
    }
}

/// A fixed two-pane report with its OS figures sampled, independent of the host.
fn fixture() -> MemoryReport {
    let panes = [retention(4), retention(1)];
    let session = PaneRetention {
        grid_visible: ResourceAmount { bytes: MIB, items: 2 },
        grid_history: ResourceAmount { bytes: 5 * MIB, items: 2 },
        parser: ResourceAmount { bytes: MIB / 2, items: 2 },
        ..PaneRetention::default()
    };
    MemoryReport {
        header: ReportHeader {
            version: "9.9.9".to_string(),
            timestamp: "2026-10-17 12:22:23 -07:00".to_string(),
            pid: 4242,
            copy_all_hint: "Cmd+Shift+C",
        },
        snapshot: MemorySnapshot {
            process: ProcessMemory {
                private_committed: MemoryMetric::Unsupported,
                resident: MemoryMetric::Bytes(600 * MIB as u64),
                virtual_bytes: MemoryMetric::Bytes(400 * 1024 * MIB as u64),
            },
            session,
            panes_total: 2,
            panes_sampled: 2,
            panes_contended: 0,
            renderers: vec![renderer("WindowId(1)")],
            trim_seq: 0,
            trim_source: None,
            allocator: None,
            live_renderers: 1,
            live_fg_probe_workers: 0,
        },
        windows: vec![WindowRow {
            label: "window 1".to_string(),
            is_report: false,
            renderer: Some(renderer("WindowId(1)")),
        }],
        panes: vec![
            PaneRow {
                label: "tab 1 \"zsh\" pane 1".to_string(),
                is_report: false,
                retention: Some(panes[0]),
            },
            PaneRow {
                label: "tab 1 \"Memory Usage\" pane 1".to_string(),
                is_report: true,
                retention: Some(panes[1]),
            },
        ],
        roots: vec![RootRow { label: "tab 1 pane 1".to_string(), pid: Some(100), start: None }],
        os: OsFigures::Sampled(OsSample {
            footprint: ProcessFootprint {
                current: MemoryMetric::Bytes(900 * MIB as u64),
                current_label: "physical footprint",
                peak: MemoryMetric::Bytes(1_400 * MIB as u64),
                peak_label: "lifetime peak footprint",
            },
            uptime_secs: Some(9 * 86_400 + 3 * 3_600 + 5),
            threads: ThreadSummary {
                total: Some(3),
                names: vec![("sonic-pty-reader".to_string(), 2), ("main".to_string(), 1)],
            },
            trees: vec![ProcessTree {
                processes: vec![
                    ChildProcess {
                        pid: 100,
                        depth: 0,
                        name: "zsh".to_string(),
                        memory: MemoryMetric::Bytes(3 * MIB as u64),
                    },
                    ChildProcess {
                        pid: 101,
                        depth: 1,
                        name: "tmux".to_string(),
                        memory: MemoryMetric::Bytes(12 * MIB as u64),
                    },
                ],
                outcome: WalkOutcome::Complete,
            }],
        }),
    }
}

/// The fixture's rendering, pinned: section order, labels, units, the signed difference and
/// the process tree layout are all part of the copied report a user pastes into an issue.
/// `{CHILD}` stands for the host's per-process figure label, the one platform-dependent word.
const GOLDEN: &str = r#"SonicTerm 9.9.9 memory report - 2026-10-17 12:22:23 -07:00 - pid 4242 - uptime 9d 3h
(Select text and copy it, or press Cmd+Shift+C for the whole report. R refreshes, Esc closes.)

PROCESS (from the OS)
  physical footprint    900.0 MB   lifetime peak footprint 1.4 GB
  resident (RSS)        600.0 MB
  private committed     not available
  virtual               400.0 GB   (reserved address space, not consumption)
  GPU                   not available yet

ACCOUNTED BY SONICTERM    27.0 MB
  renderers               20.5 MB
  panes                   6.5 MB
DIFFERENCE                +873.0 MB   (OS footprint minus SonicTerm's counted capacity: a signed diagnostic,
                             not a reconciliation; allocator slack, GPU, fonts and untracked allocations
                             land here, and the figures can overlap)
SAMPLE                    2 of 2 panes measured, 0 busy; taken 2026-10-17 12:22:23 -07:00

WINDOWS / RENDERERS
  window 1                total 20.5 MB
    glyph atlas           16.0 MB   2048x2048, 9,812 entries, fit=1024
    image atlas           0.0 MB   0 images
    row glyph cache       3.0 MB   1,204 rows
    row quad cache        1.0 MB   1,204 rows
    row ink               0.0 MB   0 rows
    frame scratch         0.0 MB   0 buffers
    vertex scratch        0.5 MB   1 buffers
    software frame        0.0 MB   0 frames
    chrome cache          0.0 MB   0 entries

PANES
  tab 1 "zsh" pane 1                      total 4.8 MB
    grid visible / history / alternate    0.5 MB / 4.0 MB / 0.0 MB
    parser / hyperlinks / inline media    0.2 MB / 0.0 MB / 0.0 MB
    pty output / input                    0.0 MB / 0.0 MB
  tab 1 "Memory Usage" pane 1             total 1.8 MB   (this report)
    grid visible / history / alternate    0.5 MB / 1.0 MB / 0.0 MB
    parser / hyperlinks / inline media    0.2 MB / 0.0 MB / 0.0 MB
    pty output / input                    0.0 MB / 0.0 MB

THREADS                   3 threads share the process's memory; a thread holds none of its own
  sonic-pty-reader x2, main

CHILD PROCESSES (each pane's shell and descendants, {CHILD}; not part of SonicTerm's footprint)
  tab 1 pane 1    zsh (100)                       3.0 MB
                    - tmux (101)                  12.0 MB
  A process that left its shell's tree (a daemonized tmux server, for example) is not listed;
  WSL and remote processes are not visible from here.
"#;

/// Contract: a fixed report renders byte-for-byte to the reviewed golden text.
#[test]
fn a_fixed_report_renders_to_the_golden_text() {
    let rendered = fixture().render();
    let expected = GOLDEN.replace("{CHILD}", CHILD_MEMORY_LABEL);
    assert_eq!(rendered, expected, "rendered report:\n{rendered}");
}

/// Contract: a pane whose lock was held prints `busy, skipped` and the sample line says how many
/// were busy, so a partial total is never presented as the whole session.
#[test]
fn a_contended_pane_prints_busy_skipped() {
    let mut report = fixture();
    report.panes[0].retention = None;
    report.snapshot.panes_sampled = 1;
    report.snapshot.panes_contended = 1;
    let text = report.render();
    let busy = text.lines().find(|line| line.contains("\"zsh\" pane 1")).expect("pane row");
    assert!(busy.ends_with("busy, skipped"), "{busy}");
    assert!(text.contains("1 of 2 panes measured, 1 busy"), "{text}");
    assert!(text.contains("the pane total is partial"), "{text}");
}

/// Contract: when SonicTerm counts more than the OS footprint, the difference keeps its minus
/// sign rather than being clamped to zero.
#[test]
fn a_negative_difference_keeps_its_sign() {
    let mut report = fixture();
    if let OsFigures::Sampled(sample) = &mut report.os {
        sample.footprint.current = MemoryMetric::Bytes(10 * MIB as u64);
    }
    let text = report.render();
    let difference = text.lines().find(|line| line.starts_with("DIFFERENCE")).expect("line");
    assert!(difference.contains("-17.0 MB"), "{difference}");
}

/// Contract: an unmeasured OS footprint prints `not available` and no invented difference.
#[test]
fn an_unmeasured_footprint_prints_not_available() {
    let mut report = fixture();
    report.os = OsFigures::Pending;
    let text = report.render();
    assert!(text.contains("sampling the OS figures"), "{text}");
    assert!(text.contains("DIFFERENCE                not available"), "{text}");
    assert!(text.contains("tab 1 pane 1    pid 100: not sampled"), "{text}");
}

/// Contract: tab titles and process names carrying ESC, OSC and BEL bytes render as plain
/// characters, so feeding the report to a parser cannot run an escape sequence.
#[test]
fn hostile_titles_and_process_names_render_as_plain_text() {
    assert_eq!(sanitize_external("a\x1b]0;pwned\x07b\u{9b}c"), "a ]0;pwned b c");
    let mut report = fixture();
    if let OsFigures::Sampled(sample) = &mut report.os {
        sample.trees[0].processes[1].name = "evil\x1b[2Jname".to_string();
        sample.threads.names[0].0 = "thread\x1b]52;c;AAAA\x07".to_string();
    }
    let text = report.render();
    assert!(!text.contains('\x1b') && !text.contains('\x07'), "{text:?}");
    assert!(text.contains("evil [2Jname"), "{text}");
}

/// Contract: the bytes fed to the pane end each line with CR LF, so every line starts at column 0.
#[test]
fn feed_bytes_use_carriage_return_line_feed() {
    assert_eq!(report_feed_bytes("a\nb\n"), b"a\r\nb\r\n".to_vec());
}

/// Contract: a truncated or cancelled walk says so after the processes it did list.
#[test]
fn a_truncated_walk_says_more_processes_exist() {
    let mut report = fixture();
    if let OsFigures::Sampled(sample) = &mut report.os {
        sample.trees[0].outcome = WalkOutcome::Truncated;
    }
    assert!(report.render().contains("more processes; the list stops at 64"));
    if let OsFigures::Sampled(sample) = &mut report.os {
        sample.trees[0].outcome = WalkOutcome::RootGone;
    }
    assert!(report.render().contains("pid 100: shell exited"));
}

/// Contract: the report window runs copy, search, scroll, font and palette actions itself;
/// closes on tab and pane close; redirects window-level actions to a terminal window; and
/// consumes pane structure, input and tab navigation so they never touch a hidden terminal.
#[test]
fn report_routes_every_action_family() {
    for action in [
        Action::CopyToClipboard,
        Action::EnterCopyMode,
        Action::OpenSearch,
        Action::Scroll(ScrollAction::PageUp),
        Action::IncreaseFontSize,
        Action::OpenCommandPalette,
        Action::ShowMemoryUsage,
        Action::CopyMemoryReport,
        Action::ToggleFullscreen,
        Action::QuitApp,
    ] {
        assert_eq!(report_route(&action), ReportRoute::Local, "{action:?}");
    }
    for action in [Action::CloseTab, Action::ClosePane, Action::CloseActivePaneOrTab] {
        assert_eq!(report_route(&action), ReportRoute::CloseReport, "{action:?}");
    }
    for action in [Action::NewTab, Action::NewWindow, Action::ReloadConfig, Action::CheckForUpdates]
    {
        assert_eq!(report_route(&action), ReportRoute::Redirect, "{action:?}");
    }
    for action in [
        Action::PasteFromClipboard,
        Action::SplitRight,
        Action::ToggleBroadcast { scope: BroadcastScope::AllTabs },
        Action::FocusPane(Direction::Left),
        Action::MoveTabToNewWindow,
        Action::NextTab,
        Action::RenameTab,
    ] {
        assert_eq!(report_route(&action), ReportRoute::Consume, "{action:?}");
    }
}

/// Contract: R refreshes, Esc closes, and only the platform's copy-all chord copies everything;
/// other keys are left for the keymap.
#[test]
fn report_keys_match_r_escape_and_the_copy_all_chord() {
    let chords =
        |spellings: &[&str]| spellings.iter().map(|text| text.to_string()).collect::<Vec<_>>();
    assert_eq!(report_key(&chords(&["r"])), Some(ReportKey::Refresh));
    assert_eq!(report_key(&chords(&["shift+r", "shift+r"])), Some(ReportKey::Refresh));
    assert_eq!(report_key(&chords(&["escape"])), Some(ReportKey::Close));
    assert_eq!(report_key(&chords(&[COPY_ALL_CHORD])), Some(ReportKey::CopyAll));
    assert_eq!(report_key(&chords(&["ctrl+r"])), None);
    assert_eq!(report_key(&chords(&["super+c"])), None);
}
