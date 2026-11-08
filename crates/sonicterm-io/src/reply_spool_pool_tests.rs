//! Pins the spool against the shared spill pool: a file's whole length is charged, a refusal is
//! the typed budget refusal that leaves the spool usable, a refusal with a small consumed prefix
//! copies nothing, every way a file goes away refunds it, and racing spools never pass capacity.

use std::sync::Arc;

use super::*;

const RECORD: u64 = (CHUNK_BYTES + HEADER_BYTES) as u64;

/// A spool on `pool` whose resident tier is already full, so its next record spills.
fn spilling(pool: &Arc<SpillPool>) -> (PtyReplySender, ReplyReader) {
    let (sender, reader) = reply_spool_with_pool(Arc::clone(pool));
    sender.send(vec![b'r'; CHUNK_BYTES]).unwrap();
    (sender, reader)
}

/// Two spools whose combined files would pass a small pool: the second is refused (typed, counted,
/// not latched) while each is far under its own 16 MiB cap; both drain in order, the pool returns
/// to empty, and the refused spool admits again.
#[test]
fn spools_share_one_budget_and_drain_back_to_empty() {
    let pool = SpillPool::new(4 * RECORD);
    let (first, first_reader) = spilling(&pool);
    let (second, second_reader) = spilling(&pool);
    for index in 0..4u8 {
        first.send(vec![index; CHUNK_BYTES]).unwrap();
    }
    assert_eq!(pool.charged(), 4 * RECORD);
    let error = second.send(vec![9; CHUNK_BYTES]).unwrap_err();
    assert!(is_budget_refusal(&error), "{error}");
    assert_eq!(second.stats().dropped_replies, 1);
    assert_eq!(first_reader.pop().unwrap().unwrap(), vec![b'r'; CHUNK_BYTES]);
    for index in 0..4u8 {
        assert_eq!(first_reader.pop().unwrap().unwrap(), vec![index; CHUNK_BYTES]);
    }
    assert_eq!(pool.charged(), 0, "the drained file refunded its whole length");
    second.send(vec![7; CHUNK_BYTES]).unwrap();
    assert_eq!(second_reader.pop().unwrap().unwrap(), vec![b'r'; CHUNK_BYTES]);
    assert_eq!(second_reader.pop().unwrap().unwrap(), vec![7; CHUNK_BYTES]);
    assert_eq!(pool.charged(), 0);
}

/// The charge is the file's length, consumed prefix included: popping records does not refund
/// them until the file drains or compacts.
#[test]
fn a_consumed_prefix_stays_charged_until_the_file_shrinks() {
    let pool = SpillPool::new(u64::MAX);
    let (sender, reader) = spilling(&pool);
    for _ in 0..3 {
        sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    }
    reader.pop().unwrap().unwrap();
    reader.pop().unwrap().unwrap();
    assert_eq!(pool.charged(), 3 * RECORD, "a consumed record is still in the file");
    reader.pop().unwrap().unwrap();
    reader.pop().unwrap().unwrap();
    assert_eq!(pool.charged(), 0);
}

/// A full pool with a consumed prefix of at least 1 MiB compacts it away and admits the record;
/// with a smaller prefix it refuses at once and copies nothing.
#[test]
fn a_full_pool_compacts_only_a_large_prefix() {
    let small_prefix = 2 * RECORD;
    let pool = SpillPool::new(small_prefix + 4 * RECORD);
    let (sender, reader) = spilling(&pool);
    for _ in 0..6 {
        sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    }
    reader.pop().unwrap().unwrap();
    reader.pop().unwrap().unwrap();
    reader.pop().unwrap().unwrap();
    let read_before = sender.state.lock().spill.as_ref().unwrap().read;
    assert!(read_before < COMPACT_MIN_BYTES);
    assert!(is_budget_refusal(&sender.send(vec![b'x'; CHUNK_BYTES]).unwrap_err()));
    assert_eq!(sender.state.lock().spill.as_ref().unwrap().read, read_before, "nothing was copied");

    let records = COMPACT_MIN_BYTES / RECORD + 2;
    let pool = SpillPool::new((records + 1) * RECORD);
    let (sender, reader) = spilling(&pool);
    for _ in 0..=records {
        sender.send(vec![b'b'; CHUNK_BYTES]).unwrap();
    }
    for _ in 0..=(COMPACT_MIN_BYTES / RECORD + 1) {
        reader.pop().unwrap().unwrap();
    }
    assert!(sender.state.lock().spill.as_ref().unwrap().read >= COMPACT_MIN_BYTES);
    sender.send(vec![b'y'; CHUNK_BYTES]).unwrap();
    assert_eq!(sender.state.lock().spill.as_ref().unwrap().read, 0, "the prefix was compacted");
    assert!(pool.charged() <= pool.capacity());
}

/// Every way a spill file goes away refunds its charge: a write failing after its reservation,
/// a failed compaction, a reader failure, a reader drop, and a close during an append.
#[test]
fn every_way_a_file_goes_away_refunds_it() {
    let pool = SpillPool::new(u64::MAX);
    let (sender, _reader) = spilling(&pool);
    sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    let error = sender
        .state
        .lock()
        .append_with(&[b'c'; 10], |_, _| Err(io::Error::other("injected write failure")))
        .unwrap_err();
    assert!(!is_budget_refusal(&error));
    assert_eq!(pool.charged(), 0, "a failed write refunded the file");

    let (sender, reader) = spilling(&pool);
    sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    reader.fail(io::Error::other("writer failed"));
    assert_eq!(pool.charged(), 0, "a reader failure refunded the file");

    let (sender, reader) = spilling(&pool);
    sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    drop(reader);
    assert_eq!(pool.charged(), 0, "a reader drop refunded the file");

    let (sender, _reader) = spilling(&pool);
    sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    let closed = Arc::clone(&sender.closed);
    let result = sender.state.lock().append_with(&[b'c'; 10], |file, bytes| {
        closed.store(true, Ordering::SeqCst);
        file.write_all(bytes)
    });
    let finished = sender.state.lock().finish_operation(result);
    assert_eq!(finished.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(pool.charged(), 0, "a close during an append released the file");
}

/// Eight spools sending from eight threads against one pool: it never passes capacity, refusals
/// are typed, and once every reader drains the pool is empty.
#[test]
fn racing_spools_never_pass_the_pool() {
    let pool = SpillPool::new(16 * RECORD);
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let pool = Arc::clone(&pool);
            std::thread::spawn(move || {
                let (sender, reader) = spilling(&pool);
                for round in 0..200 {
                    match sender.send(vec![b'q'; CHUNK_BYTES]) {
                        Ok(()) => {}
                        Err(error) => assert!(is_budget_refusal(&error), "{error}"),
                    }
                    if round % 3 == 0 {
                        reader.pop().unwrap();
                    }
                }
                while reader.pop().unwrap().is_some() {}
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert!(pool.peak() <= pool.capacity(), "peak {} over {}", pool.peak(), pool.capacity());
    assert_eq!(pool.charged(), 0);
}
