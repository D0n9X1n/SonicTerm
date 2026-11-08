//! Pins the summed queue footprint: exact totals per category, disk kept apart from memory.

use sonicterm_io::pty::PtyQueueFootprint;

use super::sum_queues;

/// Four panes' footprints add up per category, and the resident total leaves the disk spill out.
#[test]
fn queue_totals_add_per_category_and_keep_disk_apart() {
    let pane = |input: u64, writer: u64, resident: u64, spill: u64| PtyQueueFootprint {
        input_queued_bytes: input,
        writer_in_flight_bytes: writer,
        reply_resident_bytes: resident,
        reply_spill_bytes: spill,
    };
    let totals = sum_queues([
        pane(100, 0, 6, 0),
        pane(0, 4096, 0, 0),
        pane(0, 0, 65_536, 1 << 20),
        pane(7, 0, 0, 0),
    ]);
    assert_eq!(totals.panes, 4);
    assert_eq!(totals.footprint.input_queued_bytes, 107);
    assert_eq!(totals.footprint.writer_in_flight_bytes, 4096);
    assert_eq!(totals.footprint.reply_resident_bytes, 65_542);
    assert_eq!(totals.footprint.reply_spill_bytes, 1 << 20);
    assert_eq!(totals.footprint.resident_bytes(), 107 + 4096 + 65_542, "disk is not resident");
}

/// A real PTY whose child switched its terminal to raw mode and stopped itself, with its writer
/// blocked partway through a large input message: in raw mode a full input buffer blocks the
/// writer (canonical mode would discard the overflow), so from here on nothing drains and every
/// byte the test adds stays where it was put.
#[cfg(unix)]
fn blocked_pty() -> sonicterm_io::pty::PtyHandle {
    let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(
        "/bin/sh",
        &["-c".into(), "stty raw -echo; echo ready; kill -STOP $$; sleep 60".into()],
        80,
        24,
    )
    .unwrap();
    // The child stops only after `stty`, so the mode is set before any input arrives.
    let ready_by = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("ready") {
        assert!(std::time::Instant::now() < ready_by, "the child set raw mode");
        if let Ok(chunk) = pty.out_rx.recv_timeout(std::time::Duration::from_millis(50)) {
            seen.extend_from_slice(&chunk);
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    // Larger than any terminal input buffer, so the writer cannot finish it.
    pty.send_input_nonblocking(vec![b'x'; 1 << 20]).unwrap();
    let blocked_by = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while pty.queue_footprint().writer_in_flight_bytes == 0 {
        assert!(std::time::Instant::now() < blocked_by, "the writer took the message");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // Still blocked after a pause: the message is stuck, not passing through.
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(pty.queue_footprint().writer_in_flight_bytes, 1 << 20, "the writer is blocked");
    pty
}

/// Four real panes with blocked writers, each given a different, known load: queued input, a
/// spilled reply backlog, resident replies, and nothing more. The App's totals are exactly the
/// bytes the test placed, per category; the in-memory total leaves the spill out; and each pane's
/// `pty_input` retention is its own resident queue bytes, so pane and session accounting cover
/// what the queue section reports.
#[cfg(unix)]
#[test]
fn the_app_sums_known_loads_on_every_pane() {
    const RAM_BYTES: u64 = 64 * 1024;
    const HEADER_BYTES: u64 = 4;
    let mut app = crate::app::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let mut pane_ids = Vec::new();
    for index in 0..4 {
        let pane_id = app.__test_seed_tab(&format!("pane-{index}"));
        app.registry.pane_mut(pane_id).unwrap().pty = Some(blocked_pty());
        pane_ids.push(pane_id);
    }
    let pty = |app: &crate::app::App, index: usize| {
        app.registry.pane(pane_ids[index]).unwrap().pty.as_ref().unwrap().queue_footprint()
    };
    let baseline: Vec<_> = (0..4).map(|index| pty(&app, index)).collect();
    // Pane 0: two queued input messages behind the blocked one.
    for _ in 0..2 {
        app.registry
            .pane(pane_ids[0])
            .unwrap()
            .pty
            .as_ref()
            .unwrap()
            .send_input_nonblocking(vec![b'q'; 3000])
            .unwrap();
    }
    // Pane 1: a reply backlog past the resident tier, so 40 replies of 2 KiB spill.
    let replies = app.registry.pane(pane_ids[1]).unwrap().pty.as_ref().unwrap().reply_sender();
    let reply_count = 72u64;
    for _ in 0..reply_count {
        replies.send(vec![b'r'; 2044]).unwrap();
    }
    // Pane 2: five replies that stay in the resident ring.
    let resident_replies =
        app.registry.pane(pane_ids[2]).unwrap().pty.as_ref().unwrap().reply_sender();
    for _ in 0..5 {
        resident_replies.send(vec![b's'; 100]).unwrap();
    }
    let totals = app.pty_queue_totals();
    let in_flight: u64 = baseline.iter().map(|footprint| footprint.writer_in_flight_bytes).sum();
    let queued_before: u64 = baseline.iter().map(|footprint| footprint.input_queued_bytes).sum();
    let resident_before: u64 =
        baseline.iter().map(|footprint| footprint.reply_resident_bytes).sum();
    assert_eq!(totals.panes, 4);
    assert_eq!(totals.footprint.input_queued_bytes, queued_before + 6000, "queued input");
    assert_eq!(totals.footprint.writer_in_flight_bytes, in_flight, "the four blocked messages");
    assert_eq!(in_flight, 4 << 20, "each writer holds its whole 1 MiB message");
    // Every 2 KiB record (2,048 framed) fills the 64 KiB ring at 32 records; the other 40 spill.
    let framed = 2044 + HEADER_BYTES;
    let spilled = (reply_count - RAM_BYTES / framed) * framed;
    assert_eq!(totals.footprint.reply_spill_bytes, spilled, "spilled replies");
    // Two rings are allocated (panes 1 and 2), each at the tier's full capacity.
    assert_eq!(totals.footprint.reply_resident_bytes, resident_before + 2 * RAM_BYTES, "rings");
    assert_eq!(
        totals.footprint.resident_bytes(),
        totals.footprint.input_queued_bytes
            + totals.footprint.writer_in_flight_bytes
            + totals.footprint.reply_resident_bytes,
        "disk spill is never part of the in-memory total"
    );
    // Pane retention charges the same resident bytes, pane by pane, and never the spill.
    for (index, pane_id) in pane_ids.iter().enumerate() {
        let footprint = pty(&app, index);
        let retention = super::super::retention::measure_pane(app.registry.pane(*pane_id).unwrap())
            .expect("an uncontended pane");
        assert_eq!(retention.pty_input.bytes as u64, footprint.resident_bytes(), "pane {index}");
    }
}
