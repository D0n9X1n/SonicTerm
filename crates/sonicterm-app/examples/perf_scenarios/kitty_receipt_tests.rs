//! The kitty receipt counts what the pane holds, and the fixture's own bytes produce the `print`
//! receipt on the smallest window the macOS runner gives.

use sonicterm_grid::grid::Grid;
use sonicterm_render_model::KittyTag;
use sonicterm_vt::vt::{CaptureStagingPool, KittyAction, KittyCommand, Parser, VtEvent};

use super::*;

/// The grid the macOS runner gives this scenario, read from PR CI's comparison table.
const RUNNER_COLS: u16 = 125;
/// Rows of that grid.
const RUNNER_ROWS: u16 = 30;

/// A one-pixel store entry for buffer `id` tagged with kitty `image_id`, `placement_id` and `show`.
fn entry(id: u64, image_id: u32, placement_id: u32, show: KittyShow) -> InlineImage {
    InlineImage {
        id,
        row: 0,
        col: 0,
        scroll_mark: 0,
        width: 1,
        height: 1,
        bgra: std::sync::Arc::from(vec![0u8; 4]),
        kitty: Some(KittyTag { image_id, placement_id, show, source_px: (1, 1) }),
    }
}

/// The store the fixture's commands leave, built the way the store applies them: each transmit
/// an upload with a fresh buffer, each placement an entry sharing its image's buffer, and a
/// placement past `cap` refused. Untagged and failed commands are not part of the fixture.
fn store_from(commands: &[KittyCommand], cap: usize) -> Vec<InlineImage> {
    let mut images: Vec<InlineImage> = Vec::new();
    for command in commands {
        match command.action {
            KittyAction::Transmit => {
                let buffer = images.len() as u64 + 1;
                images.push(entry(buffer, command.image_id, 0, KittyShow::Upload));
            }
            KittyAction::Place => {
                let placed =
                    images.iter().filter(|image| image.kitty.unwrap().show != KittyShow::Upload);
                if placed.count() >= cap {
                    // When: the pane already holds `cap` placements, the store refuses this one.
                    continue;
                }
                let upload = images
                    .iter()
                    .find(|image| image.kitty.unwrap().image_id == command.image_id)
                    .expect("a placement names a transmitted image")
                    .id;
                let show = KittyShow::Virtual { cols: command.cols, rows: command.rows };
                images.push(entry(upload, command.image_id, command.placement_id, show));
            }
            _ => panic!("the fixture sends only transmits and placements"),
        }
    }
    images
}

/// Feed `bytes` through the real parser on a `cols` x `rows` grid with the pane tty's newline
/// translation, returning the parser and the kitty commands it emitted in stream order.
fn parse(bytes: &[u8], cols: u16, rows: u16) -> (Parser, Vec<KittyCommand>) {
    let translated: Vec<u8> = bytes
        .iter()
        .flat_map(|byte| if *byte == b'\n' { vec![b'\r', b'\n'] } else { vec![*byte] })
        .collect();
    let mut parser =
        Parser::new_with_staging_pool(Grid::new(cols, rows), None, CaptureStagingPool::new());
    let commands = parser
        .advance(&translated)
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::Media(media) => KittyCommand::parse(media.metadata.as_bytes()).ok(),
            _ => None,
        })
        .collect();
    (parser, commands)
}

/// The receipt of `parser`'s whole visible grid over `images` against `phase`'s placements.
fn grid_receipt(parser: &Parser, images: &[InlineImage], phase: &str) -> KittyReceipt {
    let grid = parser.grid();
    receipt(images, (0..grid.rows).map(|row| grid.row(row)), placements_per_image(phase))
}

#[test]
fn the_print_fixture_holds_its_receipt_on_the_smallest_runner_window() {
    // The macOS runner gives this scenario a 125x30 grid: with the READY line above the fixture and
    // the sentinel and a prompt below it, every one of the 512 runs must still be visible and name a
    // placement the pane holds. A 32-row layout lost four rows there (448 runs).
    let mut bytes = b"READY 0\n".to_vec();
    bytes.extend(crate::workload::kitty_placeholders());
    bytes.extend_from_slice(b"PERF_DONE 0 0000000000000000\n$ ");
    let (parser, commands) = parse(&bytes, RUNNER_COLS, RUNNER_ROWS);
    let cap = crate::workload::KITTY_PLACEMENT_CAP as usize;
    let images = store_from(&commands, cap);
    assert_eq!(Some(grid_receipt(&parser, &images, "print")), expected("print"));
}

#[test]
fn the_cap_command_fills_the_cap_and_one_is_refused() {
    // The `cap` phase's shell line, expanded as zsh prints it, adds 512 placements to the fixture's
    // 512 and one past the 1,024 cap, which is refused; the drawn runs stay resolved.
    let mut bytes = b"READY 0\n".to_vec();
    bytes.extend(crate::workload::kitty_placeholders());
    for image_id in 1..=crate::workload::KITTY_IMAGES {
        for placement_id in 5..=8 {
            bytes.extend(format!("\x1b_Ga=p,U=1,i={image_id},p={placement_id},q=2\x1b\\").bytes());
        }
    }
    bytes.extend_from_slice(b"\x1b_Ga=p,U=1,i=1,p=9,q=2\x1b\\");
    // The sentinel, the prompt with the typed characters, the `cap` command line as the shell echoes
    // it, and a new prompt follow the rows on screen, as they do on the runner.
    let line = match crate::scenarios::KITTY_CAP_STEPS[0].text {
        crate::scenarios::CommitText::Literal(line) => line.trim_start_matches('\u{15}').trim_end(),
        crate::scenarios::CommitText::Repeat(..) => panic!("the cap step is a literal line"),
    };
    let typed = "x".repeat(50);
    bytes.extend(format!("PERF_DONE 0 0000000000000000\n$ {typed}{line}\n$ ").bytes());
    let (parser, commands) = parse(&bytes, RUNNER_COLS, RUNNER_ROWS);
    let cap = crate::workload::KITTY_PLACEMENT_CAP as usize;
    let images = store_from(&commands, cap);
    let places = commands.iter().filter(|command| command.action == KittyAction::Place).count();
    assert_eq!(places, cap + 1, "one placement is sent past the cap");
    assert_eq!(Some(grid_receipt(&parser, &images, "cap")), expected("cap"));
    // The shell line the plan commits names exactly these placements.
    let line = match crate::scenarios::KITTY_CAP_STEPS[0].text {
        crate::scenarios::CommitText::Literal(line) => line,
        crate::scenarios::CommitText::Repeat(..) => panic!("the cap step is a literal line"),
    };
    assert!(line.contains("for i in {1..128}; do for p in 5 6 7 8;") && line.contains("i=1,p=9"));
}

#[test]
fn a_refused_or_missing_image_changes_the_receipt() {
    // The receipt counts distinct retained buffers, non-upload placements and only runs naming a
    // virtual placement the pane holds, so a missing upload, a refused placement or a run naming an
    // absent placement each show.
    let images = [
        entry(1, 7, 0, KittyShow::Upload),
        entry(1, 7, 1, KittyShow::Virtual { cols: 4, rows: 1 }),
        entry(1, 7, 2, KittyShow::Anchored { cols: 0, rows: 0 }),
        entry(2, 8, 0, KittyShow::Upload),
    ];
    let mut bytes = Vec::new();
    for (image_id, placement_id) in [(7, 1), (7, 3)] {
        bytes.extend(format!("\x1b[38;2;0;0;{image_id}m\x1b[58;2;0;0;{placement_id}m").bytes());
        bytes.extend("\u{10EEEE}\u{0305}\u{0305}\x1b[39;59m ".bytes());
    }
    let (parser, _) = parse(&bytes, 20, 2);
    let got = receipt(&images, (0..2).map(|row| parser.grid().row(row)), 2);
    assert_eq!(got, KittyReceipt { images: 2, placements: 2, runs: 1, exact: false });
}

#[test]
fn only_the_planned_placement_identities_are_exact() {
    // Totals cannot tell the cap's refusal apart from losing another placement while admitting the
    // one past it: the identity check can. Each case keeps 1,024 placements and 128 images.
    let mut placements = Vec::new();
    for image_id in 1..=crate::workload::KITTY_IMAGES {
        placements.push(entry(u64::from(image_id), image_id, 0, KittyShow::Upload));
        for placement_id in 1..=8 {
            let show = KittyShow::Virtual { cols: 4, rows: 1 };
            placements.push(entry(u64::from(image_id), image_id, placement_id, show));
        }
    }
    let empty = Grid::new(4, 1);
    let rows = || (0..1).map(|row| empty.row(row));
    assert!(receipt(&placements, rows(), 8).exact, "the planned placements are exact");
    // The one past the cap admitted in place of placement 8 of image 2.
    let swapped: Vec<InlineImage> = placements
        .iter()
        .map(|image| match image.kitty {
            Some(tag) if tag.image_id == 2 && tag.placement_id == 8 => {
                entry(1, 1, 9, KittyShow::Virtual { cols: 4, rows: 1 })
            }
            _ => image.clone(),
        })
        .collect();
    let got = receipt(&swapped, rows(), 8);
    assert_eq!((got.placements, got.exact), (1_024, false), "same total, wrong identities");
    // A placement held twice in place of another is not exact either.
    let doubled: Vec<InlineImage> = placements
        .iter()
        .map(|image| match image.kitty {
            Some(tag) if tag.image_id == 3 && tag.placement_id == 8 => {
                entry(3, 3, 7, KittyShow::Virtual { cols: 4, rows: 1 })
            }
            _ => image.clone(),
        })
        .collect();
    assert!(!receipt(&doubled, rows(), 8).exact, "a duplicate is not exact");
}

#[test]
fn each_phase_expects_its_store_commands() {
    // `print` answers every transmit and drawn placement; `cap` its 512 placements and the refused
    // one, so a run that never sent the refused command is caught by the sample count.
    assert_eq!(store_commands("print"), Some(128 + 512));
    assert_eq!(store_commands("cap"), Some(513));
    assert_eq!(store_commands("typing"), None);
    assert_eq!((placements_per_image("print"), placements_per_image("cap")), (4, 8));
}
