//! Regression contracts for this responsibility, sharing the existing fixture state.

use super::super::workload_tests::*;
use super::*;

#[test]
fn seeded_rng_matches_reference_splitmix64() {
    // Every fixture derives from this generator; the values come from an independent implementation.
    let mut zero = SeededRng::new(0);
    assert_eq!(
        [zero.next_u64(), zero.next_u64(), zero.next_u64()],
        [0xe220a8397b1dcdaf, 0x6e789e6aa1b965f4, 0x06c45d188009454f]
    );
    let mut other = SeededRng::new(1_234_567);
    assert_eq!(
        [other.next_u64(), other.next_u64(), other.next_u64()],
        [0x599ed017fb08fc85, 0x2c73f08458540fa5, 0x883ebce5a3f27c77]
    );
    let mut bounded = SeededRng::new(7);
    assert!((0..1_000).all(|_| bounded.below(36) < 36));
}

#[test]
fn idle_role_script_is_pinned_line_for_line() {
    // The anchor, session record, acknowledgement and GO handshake are the cleanup contract.
    let idle = plan("S1", "default", false).unwrap();
    assert_eq!(role_script(&idle, SCRATCH, NONCE), IDLE_SCRIPT);
}

#[test]
fn every_role_script_is_pinned() {
    // A changed script changes what a scenario measures, so every plan's script is pinned.
    let actual: BTreeMap<String, String> = all_plans()
        .iter()
        .map(|plan| (plan_key(plan), sha256_hex(role_script(plan, SCRATCH, NONCE).as_bytes())))
        .collect();
    let expected: BTreeMap<String, String> =
        ROLE_SCRIPT_PINS.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect();
    assert_eq!(actual, expected, "ROLE_SCRIPT_PINS:\n{}", render_pins(&actual));
}

#[test]
fn every_fixture_is_pinned() {
    // Fixtures are generated, never stored, so their bytes are pinned by hash.
    let actual: BTreeMap<String, String> =
        fixture_sets().iter().map(|(key, files)| (key.clone(), fixture_sha256(files))).collect();
    let expected: BTreeMap<String, String> =
        FIXTURE_PINS.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect();
    assert_eq!(actual, expected, "FIXTURE_PINS:\n{}", render_pins(&actual));
}

#[test]
fn fixtures_never_contain_protocol_text() {
    // A fixture holding a sentinel, a READY line or the prompt would end a phase early.
    for (key, files) in fixture_sets() {
        for file in files {
            for needle in [&b"PERF_DONE"[..], b"READY", PROMPT.as_bytes()] {
                assert!(!file.contains(needle), "{key} {} holds {needle:?}", file.relative_path);
            }
        }
    }
}

#[test]
fn bulk_fixture_is_whole_printable_lines_of_the_requested_size() {
    // S3 counts its bytes, so the 50 MiB (5 MiB short) fixture must be exactly that long.
    for (short, mebibytes) in [(false, 50), (true, 5)] {
        let flood = plan("S3", "default", short).unwrap();
        let files = fixtures(&flood);
        let [bulk] = files.as_slice() else { panic!("S3 writes one fixture") };
        assert_eq!(bulk.relative_path, "bulk.txt");
        assert_eq!(bulk.byte_len(), mebibytes << 20);
        let FixtureBody::Repeated { block, count } = &bulk.body else { panic!("bulk repeats") };
        assert_eq!((block.len(), *count), (1 << 20, mebibytes));
        assert_eq!(block.last(), Some(&b'\n'));
        for line in block[..block.len() - 1].split(|byte| *byte == b'\n') {
            assert_eq!(line.len(), 127);
            assert!(line.iter().all(|byte| (0x20..=0x7e).contains(byte)));
        }
    }
}

#[test]
fn frame_fixtures_enter_and_leave_the_alternate_screen() {
    // S10 plays full-screen streams; sync wraps every frame in DEC 2026 brackets and nothing else does.
    for (variant, synchronized) in [("default", false), ("sync", true)] {
        let stream = plan("S10", variant, true).unwrap();
        let frames: Vec<_> = fixtures(&stream)
            .into_iter()
            .map(|fixture| {
                let FixtureBody::Bytes(bytes) = fixture.body else { panic!("frames are bytes") };
                (fixture.relative_path, bytes)
            })
            .collect();
        assert_eq!(frames.len(), 300);
        for (index, (path, bytes)) in frames.iter().enumerate() {
            assert_eq!(*path, format!("frames/{index}"));
            let bracketed = bytes.starts_with(b"\x1b[?2026h") && bytes.ends_with(b"\x1b[?2026l");
            assert_eq!(bracketed, synchronized, "{variant} frame {index}");
            let inner = if synchronized { &bytes[8..bytes.len() - 8] } else { &bytes[..] };
            assert!(!inner.windows(7).any(|window| window == b"\x1b[?2026"), "nested bracket");
        }
        let first = &frames[0].1[if synchronized { 8 } else { 0 }..];
        assert!(first.starts_with(b"\x1b[?1049h\x1b[?7l\x1b[2J"));
        let last = &frames[299].1;
        let last = &last[..last.len() - if synchronized { 8 } else { 0 }];
        assert!(last.ends_with(b"\x1b[?7h\x1b[?1049l"));
    }
}

#[test]
fn sixel_fixture_arrives_as_one_capture() {
    // S11 waits for a registered image, so its fixture must be one complete Sixel DCS.
    use sonicterm_grid::grid::Grid;
    use sonicterm_vt::vt::{CaptureStagingPool, MediaProtocol, Parser, VtEvent};
    let image = plan_for("S11", "default", false, Host::Posix).unwrap();
    let files = fixtures(&image);
    let [sixel] = files.as_slice() else { panic!("S11 writes one fixture") };
    let FixtureBody::Bytes(bytes) = &sixel.body else { panic!("sixel is bytes") };
    let mut parser =
        Parser::new_with_staging_pool(Grid::new(250, 70), None, CaptureStagingPool::new());
    let media: Vec<_> = parser
        .advance(bytes)
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::Media(media) => Some(media),
            _ => None,
        })
        .collect();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].protocol, MediaProtocol::Sixel);
    assert!(media[0].data.contains(&b'#') && media[0].data.len() > 100);
}

#[test]
fn nonce_avoids_every_fixture_including_repeated_seams() {
    // The sentinel nonce must occur nowhere in a fixture, even across a repeated block's seam.
    let mut rng = SeededRng::new(42);
    let first = format!("{:016x}", rng.next_u64());
    let second = format!("{:016x}", rng.next_u64());
    let clean =
        FixtureFile { relative_path: "clean.txt".into(), body: FixtureBody::Bytes(b"x".to_vec()) };
    assert_eq!(choose_nonce(42, std::slice::from_ref(&clean)), first);
    let body = FixtureBody::Bytes(format!("a{first}b").into_bytes());
    let colliding = FixtureFile { relative_path: "collide.txt".into(), body };
    assert_eq!(choose_nonce(42, &[clean, colliding]), second);
    let (head, tail) = first.split_at(8);
    let block = format!("{tail}----{head}").into_bytes();
    let seam = FixtureFile {
        relative_path: "seam.txt".into(),
        body: FixtureBody::Repeated { block, count: 2 },
    };
    assert!(seam.contains(first.as_bytes()));
    assert_eq!(choose_nonce(42, &[seam]), second);
}

#[test]
fn written_fixtures_match_their_pins() {
    // `write_under` must lay out exactly the bytes the pins hash, including repeated blocks.
    let dir = scratch_dir("fixtures");
    for (scenario, variant, short) in
        [("S9", "default", false), ("S3", "default", true), ("S10", "sync", true)]
    {
        for fixture in fixtures(&plan(scenario, variant, short).unwrap()) {
            fixture.write_under(&dir).unwrap();
            let written = std::fs::read(dir.join(&fixture.relative_path)).unwrap();
            assert_eq!(sha256_hex(&written), fixture_sha256(std::slice::from_ref(&fixture)));
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn image_burst_holds_ten_decodable_pngs_and_its_twin_the_same_text() {
    // S11/burst's comparison against S11/burst-text is valid only if the text is identical and the
    // burst adds exactly ten OSC 1337 images, the first past the 1,024-pixel cap so it resizes.
    const OSC_START: &[u8] = b"\x1b]1337;File=";
    let burst = image_burst(true);
    let text = image_burst(false);
    let osc_count = burst.windows(OSC_START.len()).filter(|window| *window == OSC_START).count();
    assert_eq!(osc_count, BURST_IMAGES);
    let mut stripped = Vec::new();
    let mut rest = burst.as_slice();
    let mut widths = Vec::new();
    while let Some(start) = rest.windows(OSC_START.len()).position(|window| window == OSC_START) {
        stripped.extend_from_slice(&rest[..start]);
        let end = start + rest[start..].iter().position(|byte| *byte == 0x07).unwrap();
        let payload_start = start + rest[start..end].iter().position(|byte| *byte == b':').unwrap();
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD
            .decode(&rest[payload_start + 1..end])
            .unwrap();
        // About 2 MiB each, and the IHDR width, big-endian at bytes 16..20.
        assert!((2_000_000..2_400_000).contains(&png.len()), "{} bytes", png.len());
        widths.push(u32::from_be_bytes(png[16..20].try_into().unwrap()));
        // Skip the BEL and the newline the OSC ends with.
        rest = &rest[end + 2..];
    }
    stripped.extend_from_slice(rest);
    assert_eq!(stripped, text, "the burst's text is its twin's");
    assert_eq!(widths[0], 2_048);
    assert!(widths[1..].iter().all(|width_px| *width_px == 1_024));
    assert_eq!(text.iter().filter(|byte| **byte == b'\n').count(), 1_000);
}

#[test]
fn kitty_placeholder_fixture_draws_512_runs_and_fills_the_cap() {
    // S11/kitty-placeholders measures what it claims only if, through the real parser, the fixture
    // sends 128 transmits, the 1,024 placements and one past them, and its 64 rows decode to 512
    // four-cell runs naming every image with placement ids 1 to 4.
    use sonicterm_grid::grid::Grid;
    use sonicterm_render_model::kitty_placeholder::decode_row;
    use sonicterm_vt::vt::{CaptureStagingPool, KittyAction, KittyCommand, Parser, VtEvent};
    // The pane's tty turns each newline into CR LF (`onlcr`), so the test does the same.
    let bytes: Vec<u8> = kitty_placeholders()
        .into_iter()
        .flat_map(|byte| if byte == b'\n' { vec![b'\r', b'\n'] } else { vec![byte] })
        .collect();
    let mut parser =
        Parser::new_with_staging_pool(Grid::new(250, 70), None, CaptureStagingPool::new());
    let mut actions = Vec::new();
    for event in parser.advance(&bytes) {
        if let VtEvent::Media(media) = event {
            let command = KittyCommand::parse(media.metadata.as_bytes()).expect("a kitty command");
            actions.push(command.action);
        }
    }
    let count = |wanted: KittyAction| actions.iter().filter(|action| **action == wanted).count();
    assert_eq!(count(KittyAction::Transmit), KITTY_IMAGES as usize);
    assert_eq!(count(KittyAction::Place), KITTY_PLACEMENT_CAP as usize + 1, "the cap and one past");
    let grid = parser.grid();
    let mut runs = Vec::new();
    for row in 0..grid.rows {
        let line = grid.row(row);
        runs.extend(decode_row(line.iter().enumerate().map(|(col, cell)| (col as u16, cell))));
    }
    assert_eq!(runs.len(), KITTY_DRAWN_PLACEMENTS as usize);
    assert!(runs.iter().all(|run| run.cell_count == 4 && run.image_row == 0 && run.image_col == 0));
    let images: std::collections::BTreeSet<u32> = runs.iter().map(|run| run.image_id).collect();
    assert_eq!(images, (1..=KITTY_IMAGES).collect());
    let placements: std::collections::BTreeSet<u32> =
        runs.iter().map(|run| run.placement_id).collect();
    assert_eq!(placements, (1..=4).collect());
}
