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
fn never_empty_spill_retains_consumed_prefix_until_drain() {
    // Bound RAM honestly: this single-file design does not reclaim consumed disk prefixes until empty.
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
