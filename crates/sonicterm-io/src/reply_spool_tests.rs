use super::*;

#[test]
fn fifo_survives_ram_spill_and_return_to_ram() {
    // Spill stays behind older RAM records even after RAM gains space.
    let (sender, reader) = reply_spool();
    sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    sender.send(vec![b'b'; CHUNK_BYTES]).unwrap();
    assert_eq!(reader.pop().unwrap().unwrap(), vec![b'a'; CHUNK_BYTES]);
    sender.send(vec![b'c'; 7]).unwrap();
    assert_eq!(reader.pop().unwrap().unwrap(), vec![b'b'; CHUNK_BYTES]);
    assert_eq!(reader.pop().unwrap().unwrap(), vec![b'c'; 7]);
    assert!(sender.state.lock().spill.is_none());
    sender.send(vec![b'd'; 19]).unwrap();
    assert_eq!(reader.pop().unwrap().unwrap(), vec![b'd'; 19]);
    assert!(reader.pop().unwrap().is_none());
}

#[test]
fn finite_sixteen_mib_backlog_has_constant_resident_storage() {
    // Native writer progress is unnecessary while finite output-before-input replies accumulate.
    let (sender, reader) = reply_spool();
    for index in 0..512 {
        sender.send(vec![(index % 251) as u8; CHUNK_BYTES]).unwrap();
        let state = sender.state.lock();
        assert!(state.ram.len() <= RAM_BYTES);
        assert!(state.ram.capacity() <= RAM_BYTES);
        assert_eq!(sender.wake.len(), 1);
    }
    for index in 0..512 {
        assert_eq!(reader.pop().unwrap().unwrap(), vec![(index % 251) as u8; CHUNK_BYTES]);
    }
    assert!(reader.pop().unwrap().is_none());
    assert!(sender.state.lock().spill.is_none());
}

#[test]
fn tiny_replies_keep_complete_boundaries_in_ram_and_spill() {
    // Every writer turn ends after a complete six-byte CPR, never after a partial ESC sequence.
    let (sender, reader) = reply_spool();
    let count = 20000;
    for _ in 0..count {
        sender.send(b"\x1b[1;1R".to_vec()).unwrap();
    }
    let mut received = 0;
    while let Some(bytes) = reader.pop().unwrap() {
        assert_eq!(bytes.len() % 6, 0);
        assert!(bytes.as_chunks::<6>().0.iter().all(|reply| reply == b"\x1b[1;1R"));
        received += bytes.len() / 6;
    }
    assert_eq!(received, count);
}

#[test]
fn reader_drop_releases_storage_with_sender_clone_alive() {
    // A retained sender must not extend temporary storage beyond reader lifetime.
    let (sender, reader) = reply_spool();
    sender.send(vec![0; CHUNK_BYTES]).unwrap();
    sender.send(vec![1; CHUNK_BYTES]).unwrap();
    drop(reader);
    let state = sender.state.lock();
    assert!(state.spill.is_none());
    assert_eq!(state.ram.capacity(), 0);
    drop(state);
    assert_eq!(sender.send(vec![2]).unwrap_err().kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn close_never_waits_for_in_flight_storage_lock() {
    // Close must return while another thread holds the spool lock; the completing operation reclaims storage.
    let (sender, _reader) = reply_spool();
    sender.send(vec![0; CHUNK_BYTES]).unwrap();
    sender.send(vec![1; CHUNK_BYTES]).unwrap();
    let mut state = sender.state.lock();
    sender.close();
    assert!(sender.closed.load(Ordering::SeqCst));
    assert_eq!(sender.send(vec![2]).unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(state.finish_operation(Ok(())).unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert!(state.spill.is_none());
    assert_eq!(state.ram.capacity(), 0);
}

#[test]
fn reader_exit_reclaims_after_close_races_the_final_operation_check() {
    // A retained producer must not pin storage when closure lands after finish_operation but before unlock.
    let (sender, reader) = reply_spool();
    sender.send(vec![0; CHUNK_BYTES]).unwrap();
    sender.send(vec![1; CHUNK_BYTES]).unwrap();
    let mut state = sender.state.lock();
    state.finish_operation(Ok(())).unwrap();
    let closed = sender.closed.clone();
    let worker = std::thread::spawn(move || drop(reader));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !closed.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    let signalled = closed.load(Ordering::SeqCst);
    drop(state);
    worker.join().unwrap();
    assert!(signalled);
    let state = sender.state.lock();
    assert!(state.spill.is_none());
    assert_eq!(state.ram.capacity(), 0);
}

#[test]
fn partial_append_failure_latches_without_publishing_prefix() {
    // A partial payload write cannot publish an incomplete length-framed record.
    let (sender, reader) = reply_spool();
    sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    sender.send(vec![b'b'; CHUNK_BYTES]).unwrap();
    let error = sender
        .state
        .lock()
        .append_with(&[b'c'; 10], |file, bytes| {
            file.write_all(&bytes[..3])?;
            Err(io::Error::new(io::ErrorKind::StorageFull, "injected full disk"))
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    assert_eq!(reader.pop().unwrap_err().kind(), io::ErrorKind::StorageFull);
    assert_eq!(sender.send(vec![b'd']).unwrap_err().kind(), io::ErrorKind::StorageFull);
    assert!(sender.state.lock().spill.is_none());
}

#[test]
fn truncated_spill_fails_without_publishing_partial_record() {
    // A damaged backing file fails closed rather than returning a partial terminal response.
    let (sender, reader) = reply_spool();
    sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    sender.send(vec![b'b'; CHUNK_BYTES]).unwrap();
    reader.pop().unwrap().unwrap();
    sender.state.lock().spill.as_mut().unwrap().file.set_len(3).unwrap();
    assert_eq!(reader.pop().unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    assert!(sender.state.lock().spill.is_none());
}

#[test]
fn oversized_submission_is_refused_without_poisoning_storage() {
    // Bounding one complete submission prevents oversized writer turns without losing later valid replies.
    let (sender, reader) = reply_spool();
    assert_eq!(
        sender.send(vec![0; CHUNK_BYTES + 1]).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    sender.send(vec![1]).unwrap();
    assert_eq!(reader.pop().unwrap().unwrap(), [1]);
}

#[test]
fn never_empty_spill_retains_a_small_consumed_prefix() {
    // A consumed prefix below the compaction minimum stays in the file until it drains or grows.
    let (sender, reader) = reply_spool();
    for _ in 0..4 {
        sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    }
    reader.pop().unwrap().unwrap();
    reader.pop().unwrap().unwrap();
    sender.send(vec![b'b'; CHUNK_BYTES]).unwrap();
    let state = sender.state.lock();
    let spill = state.spill.as_ref().unwrap();
    assert_eq!(spill.read, (CHUNK_BYTES + HEADER_BYTES) as u64);
    assert_eq!(spill.file.metadata().unwrap().len(), (4 * (CHUNK_BYTES + HEADER_BYTES)) as u64);
}

/// A program that never reads its input cannot grow the spill past its disk budget: later replies
/// are refused alone and counted, the spool is not failed, and every admitted record still pops
/// in order once the reader resumes.
#[test]
fn unread_backlog_stops_at_the_disk_budget_without_failing() {
    let (sender, reader) = reply_spool();
    let record = (CHUNK_BYTES + HEADER_BYTES) as u64;
    let mut admitted = 0u32;
    let mut refused = 0u32;
    for index in 0..1024u32 {
        match sender.send(vec![(index % 251) as u8; CHUNK_BYTES]) {
            Ok(()) => admitted += 1,
            Err(error) => {
                assert!(is_budget_refusal(&error), "{error}");
                refused += 1;
            }
        }
        let state = sender.state.lock();
        if let Some(spill) = state.spill.as_ref() {
            assert!(spill.file.metadata().unwrap().len() <= SPILL_BUDGET_BYTES);
        }
    }
    // One record sits in the resident tier; the spill holds as many whole records as the budget allows.
    assert_eq!(u64::from(admitted), 1 + SPILL_BUDGET_BYTES / record);
    assert_eq!(admitted + refused, 1024);
    assert_eq!(sender.stats().dropped_replies, u64::from(refused));
    assert_eq!(sender.stats().peak_live_bytes, SPILL_BUDGET_BYTES / record * record);
    for index in 0..admitted {
        assert_eq!(reader.pop().unwrap().unwrap(), vec![(index % 251) as u8; CHUNK_BYTES]);
    }
    assert!(reader.pop().unwrap().is_none());
    // Drained, the spool admits again.
    sender.send(vec![7]).unwrap();
    assert_eq!(reader.pop().unwrap().unwrap(), [7]);
}

/// A reader that keeps up only partly never lets the file grow without bound: once the consumed
/// prefix dominates, compaction moves the live records to the front, in order.
#[test]
fn a_slow_reader_keeps_the_spill_file_bounded_and_ordered() {
    let (sender, reader) = reply_spool();
    let mut next_sent = 0u32;
    let mut next_read = 0u32;
    let mut largest_file = 0;
    // Two sends per read: the backlog grows until the budget refuses, then holds at the budget.
    for _ in 0..4000 {
        for _ in 0..2 {
            if sender.send(vec![(next_sent % 251) as u8; CHUNK_BYTES]).is_ok() {
                next_sent += 1;
            }
        }
        assert_eq!(reader.pop().unwrap().unwrap(), vec![(next_read % 251) as u8; CHUNK_BYTES]);
        next_read += 1;
        if let Some(spill) = sender.state.lock().spill.as_ref() {
            largest_file = largest_file.max(spill.file.metadata().unwrap().len());
        }
    }
    let record = (CHUNK_BYTES + HEADER_BYTES) as u64;
    assert!(largest_file <= 2 * SPILL_BUDGET_BYTES + record, "spill file grew to {largest_file}");
    while let Some(bytes) = reader.pop().unwrap() {
        assert_eq!(bytes, vec![(next_read % 251) as u8; CHUNK_BYTES]);
        next_read += 1;
    }
    assert_eq!(next_read, next_sent);
}

/// A failed or dropped reader frees the backlog, and the gauges a held sender reads say so: no
/// resident ring and no live spill are reported after the storage is gone.
#[test]
fn a_failed_or_dropped_reader_zeroes_the_gauges() {
    for drop_reader in [false, true] {
        let (sender, reader) = reply_spool();
        // Past the resident tier, so both the ring and the spill file hold bytes.
        while sender.stats().live_bytes == 0 {
            sender.send(vec![1u8; CHUNK_BYTES]).unwrap();
        }
        assert!(sender.stats().resident_bytes > 0);
        if drop_reader {
            drop(reader);
        } else {
            reader.fail(io::Error::other("writer failed"));
        }
        let stats = sender.stats();
        assert_eq!(
            (stats.live_bytes, stats.resident_bytes),
            (0, 0),
            "drop={drop_reader}: {stats:?}"
        );
    }
}

/// `stats` never takes the spool lock, so a sampler is not stuck behind a send or pop blocked in
/// file I/O; and the resident figure is the allocation a drain keeps, not the bytes still queued.
#[test]
fn stats_read_without_the_lock_and_count_retained_capacity() {
    let (sender, reader) = reply_spool();
    sender.send(vec![7u8; 100]).unwrap();
    assert_eq!(reader.pop().unwrap().unwrap(), vec![7u8; 100]);
    let held = sender.state.lock();
    let (done, finished) = std::sync::mpsc::channel();
    let sampler = {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let stats = sender.stats();
            done.send(()).unwrap();
            stats
        })
    };
    // The lock is held, as a stalled file operation would hold it; the sample still returns.
    finished
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("stats did not wait on the lock");
    drop(held);
    let stats = sampler.join().unwrap();
    assert!(stats.resident_bytes >= 104, "the drained ring keeps its allocation: {stats:?}");
    assert_eq!(stats.live_bytes, 0);
}

/// A compaction whose file I/O fails latches the spool like any storage failure: the error is
/// returned, later sends and pops report it, and the storage is released.
#[test]
fn a_failed_compaction_latches_the_spool() {
    let (sender, reader) = reply_spool();
    let record = (CHUNK_BYTES + HEADER_BYTES) as u64;
    // Fill past the compaction minimum, then consume more than half so the next spill compacts.
    let spilled = (2 * COMPACT_MIN_BYTES / record + 2) as usize;
    for _ in 0..=spilled {
        sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    }
    for _ in 0..(spilled * 3 / 4) {
        reader.pop().unwrap().unwrap();
    }
    {
        let mut state = sender.state.lock();
        let spill = state.spill.as_mut().unwrap();
        assert!(spill.read >= COMPACT_MIN_BYTES && spill.read * 2 >= spill.written);
        // A read-only handle makes the compaction's first write fail.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("spill");
        std::fs::write(&path, vec![0u8; spill.written as usize]).unwrap();
        spill.file = File::open(&path).unwrap();
    }
    let error = sender.send(vec![b'b'; CHUNK_BYTES]).unwrap_err();
    assert!(!is_budget_refusal(&error));
    assert_eq!(sender.send(vec![b'c']).unwrap_err().kind(), error.kind());
    assert_eq!(reader.pop().unwrap_err().kind(), error.kind());
    assert!(sender.state.lock().spill.is_none());
}

/// A seeded mix of record sizes, from one byte to a full chunk, with a reader that falls behind
/// and catches up: every accepted record comes back once, in order and whole (a pop may join
/// several, never split one); a refusal is the typed budget refusal; live spill never exceeds the
/// budget; the file never passes twice it.
#[test]
fn random_record_sizes_stay_ordered_and_within_the_budget() {
    let (sender, reader) = reply_spool();
    let mut seed = 0x0DD5_EED5_u64;
    let mut random = move |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let mut expected = std::collections::VecDeque::new();
    let mut next_tag = 0u8;
    let mut largest_file = 0;
    let mut refused = 0;
    for round in 0..3000 {
        // Bursts of sends, then a reader that takes a random share of the backlog. The first
        // thousand rounds read rarely, so the backlog reaches the budget before it drains.
        for _ in 0..random(6) {
            let size = 1 + random(CHUNK_BYTES);
            let record = vec![next_tag; size];
            match sender.send(record.clone()) {
                Ok(()) => {
                    expected.push_back(record);
                    next_tag = next_tag.wrapping_add(1);
                }
                Err(error) => {
                    assert!(is_budget_refusal(&error), "round {round}: {error}");
                    refused += 1;
                }
            }
        }
        let reads = if round < 1000 { usize::from(random(8) == 0) } else { random(5) };
        for _ in 0..reads {
            match reader.pop().unwrap() {
                Some(bytes) => take_whole_records(&bytes, &mut expected, round),
                None => assert!(expected.is_empty(), "round {round}: a record went missing"),
            }
        }
        let stats = sender.stats();
        assert!(stats.live_bytes <= SPILL_BUDGET_BYTES, "round {round}: {stats:?}");
        if let Some(spill) = sender.state.lock().spill.as_ref() {
            largest_file = largest_file.max(spill.file.metadata().unwrap().len());
        }
    }
    let record = (CHUNK_BYTES + HEADER_BYTES) as u64;
    assert!(largest_file <= 2 * SPILL_BUDGET_BYTES + record, "spill file grew to {largest_file}");
    assert!(refused > 0, "the backlog reached the budget");
    while let Some(bytes) = reader.pop().unwrap() {
        take_whole_records(&bytes, &mut expected, usize::MAX);
    }
    assert!(expected.is_empty(), "every accepted record was read back");
}

/// Match one pop's `bytes` against the oldest `expected` records: it must be exactly a run of
/// whole records, in order, which it removes.
fn take_whole_records(
    bytes: &[u8],
    expected: &mut std::collections::VecDeque<Vec<u8>>,
    round: usize,
) {
    let mut rest = bytes;
    while !rest.is_empty() {
        let record = expected.pop_front().unwrap_or_else(|| panic!("round {round}: extra bytes"));
        assert!(rest.starts_with(&record), "round {round}: a record was split or reordered");
        rest = &rest[record.len()..];
    }
}
