//! Guard-correlation transport, harness side: after a phase's endpoints are latched, one take of the App's
//! raw guard-correlation records is streamed to `guard-correlation/<phase_index>-<phase_name>.json` and the
//! phase record gains `guard_correlation: {status, file, take_seq, bytes, sha256, window}`. The App's
//! transferred buffers are serialized in place through borrowed adapters; no harness copy of them is made.
//!
//! Every call into the App's take, its types and the shared clock epoch sit behind
//! `#[cfg(perf_guard_spans_api)]`. perf-compare passes that cfg to both sides only when both trees define the
//! take, so the overlaid harness also builds on a tree that predates it; that build compiles the no-call
//! fallback, whose phases record `unavailable`. Nothing here correlates: perf-compare joins the sidecars.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::digest::Sha256;

/// Whether this build calls the App's guard-correlation take: the effective cfg.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const API_ENABLED: bool = cfg!(perf_guard_spans_api);

/// The sidecar's window label: it is not an atomic counter-snapshot window nor a completion window.
pub(crate) const WINDOW_LABEL: &str = "phase start to counter-end observation";

/// The scratch directory the sidecars are written to; perf-compare keeps it in the evidence.
pub(crate) const SIDECAR_DIR: &str = "guard-correlation";

/// The sidecar's frozen schema version.
pub(crate) const SIDECAR_SCHEMA: u32 = 1;

/// The single write buffer the sidecar streams through; the digest is taken from the same bytes.
pub(crate) const WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// What one phase's transport did, as the phase record names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TransportStatus {
    /// The sidecar was written and renamed into place.
    Written,
    /// This build lacks `perf_guard_spans_api`, so the App was never called.
    Unavailable,
    /// The App's counter gate is off.
    GateOff,
    /// The App's take sequence is exhausted.
    TakeExhausted,
    /// The sidecar could not be written; no file is named.
    WriteFailed,
}

/// The latched phase window as the phase record and the sidecar header both carry it; an endpoint whose
/// conversion from the shared epoch failed is `null`, which perf-compare reads as an incomplete window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct WindowRecord {
    pub(crate) label: &'static str,
    pub(crate) start_ns: Option<u64>,
    pub(crate) end_ns: Option<u64>,
}

impl From<Window> for WindowRecord {
    fn from(window: Window) -> Self {
        Self { label: WINDOW_LABEL, start_ns: window.start_ns, end_ns: window.end_ns }
    }
}

/// The phase record's `guard_correlation` field; every key is written, `null` when it does not apply.
/// `window` is written for every status, so perf-compare binds each sidecar to its own phase's latched window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct GuardCorrelationField {
    pub(crate) status: TransportStatus,
    /// The sidecar's path relative to the run's scratch, only when `written`.
    pub(crate) file: Option<String>,
    /// The take's identity when a take was issued (`written`, `write_failed`).
    pub(crate) take_seq: Option<u64>,
    /// The sidecar's size in bytes, only when `written`.
    pub(crate) bytes: Option<u64>,
    /// The lowercase hex SHA-256 of the sidecar's bytes, only when `written`.
    pub(crate) sha256: Option<String>,
    /// The phase's latched window, converted from the shared epoch.
    pub(crate) window: WindowRecord,
}

impl GuardCorrelationField {
    /// A field carrying only `status` and the window.
    fn status_only(status: TransportStatus, window: WindowRecord) -> Self {
        Self { status, file: None, take_seq: None, bytes: None, sha256: None, window }
    }
}

/// A seed that differs between runs: wall-clock nanoseconds XOR the process id.
fn nonce_seed() -> u64 {
    let nanos =
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos() as u64);
    nanos ^ u64::from(std::process::id())
}

/// The run nonce from `seed` and `hasher_word(seed)`: 32 lowercase hex characters. It resists accidental
/// collision between runs; it is not guaranteed unique, and its job is binding, not secrecy.
pub(crate) fn mint_run_nonce(seed: u64, hasher_word: impl FnOnce(u64) -> u64) -> String {
    format!("{seed:016x}{:016x}", hasher_word(seed))
}

/// The process's OS-keyed SipHash of `seed`, from a fresh `RandomState`.
fn random_state_word(seed: u64) -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(seed);
    hasher.finish()
}

/// The nonce held in `cell`, minted by `mint` on first use and never again.
pub(crate) fn run_nonce_in(cell: &OnceLock<String>, mint: impl FnOnce() -> String) -> &str {
    cell.get_or_init(mint)
}

/// This process's run nonce, minted on first use inside a transport, after every endpoint is latched. It is
/// separate from the workload's sentinel nonce, which is unchanged.
pub(crate) fn run_nonce() -> &'static str {
    static RUN_NONCE: OnceLock<String> = OnceLock::new();
    run_nonce_in(&RUN_NONCE, || mint_run_nonce(nonce_seed(), random_state_word))
}

/// A take's transferred records, serialized in place: the App's buffers through borrowed adapters, never
/// copied into harness arrays first.
pub(crate) trait TakenRecords {
    /// The take's identity.
    fn take_seq(&self) -> u64;
    /// When the take read its clock; `None` when the read failed.
    fn taken_at_ns(&self) -> Option<u64>;
    /// Write `panes`, every field of each pane transfer by name.
    fn serialize_panes<Out: serde::Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error>;
    /// Write `spans`, every field of the span batch by name.
    fn serialize_spans<Out: serde::Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error>;
}

/// The harness's reading of one take. `Unavailable` is this build's own answer when it lacks the cfg.
// A build without the cfg constructs only `Unavailable`, one with it every other variant; tests construct all.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GuardTake<Records> {
    Unavailable,
    GateOff,
    TakeExhausted,
    Taken(Records),
}

/// The latched phase window, in nanoseconds from the shared epoch; `None` when a conversion failed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) start_ns: Option<u64>,
    pub(crate) end_ns: Option<u64>,
}

/// What binds a sidecar to its run and phase.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Binding<'run> {
    pub(crate) run_nonce: &'run str,
    /// `None` in an unmanaged run; the sidecar writes `null` and perf-compare reads the run as unbound.
    pub(crate) harness_hash: Option<&'run str>,
    pub(crate) pid: u32,
    pub(crate) phase_index: usize,
    pub(crate) phase_name: &'run str,
    pub(crate) window: Window,
}

/// The sidecar's `clock` identity.
#[derive(Serialize)]
struct ClockHeader<'run> {
    pid: u32,
    run_nonce: &'run str,
}

/// `panes`, delegated to the records' own borrowed adapter.
struct PanesField<'run, Records>(&'run Records);

impl<Records: TakenRecords> Serialize for PanesField<'_, Records> {
    fn serialize<Out: serde::Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
        self.0.serialize_panes(out)
    }
}

/// `spans`, delegated to the records' own borrowed adapter.
struct SpansField<'run, Records>(&'run Records);

impl<Records: TakenRecords> Serialize for SpansField<'_, Records> {
    fn serialize<Out: serde::Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
        self.0.serialize_spans(out)
    }
}

/// The whole sidecar in its frozen key order: the header, then `panes` and `spans` from the records in place.
struct Sidecar<'run, Records> {
    binding: &'run Binding<'run>,
    records: &'run Records,
}

impl<Records: TakenRecords> Serialize for Sidecar<'_, Records> {
    fn serialize<Out: serde::Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
        use serde::ser::SerializeStruct;
        let binding = self.binding;
        let mut document = out.serialize_struct("Sidecar", 11)?;
        document.serialize_field("schema", &SIDECAR_SCHEMA)?;
        document.serialize_field("run_nonce", binding.run_nonce)?;
        document.serialize_field("harness_hash", &binding.harness_hash)?;
        document.serialize_field("phase_index", &binding.phase_index)?;
        document.serialize_field("phase_name", binding.phase_name)?;
        document.serialize_field("take_seq", &self.records.take_seq())?;
        let clock = ClockHeader { pid: binding.pid, run_nonce: binding.run_nonce };
        document.serialize_field("clock", &clock)?;
        document.serialize_field("window", &WindowRecord::from(binding.window))?;
        document.serialize_field("taken_at_ns", &self.records.taken_at_ns())?;
        document.serialize_field("panes", &PanesField(self.records))?;
        document.serialize_field("spans", &SpansField(self.records))?;
        document.end()
    }
}

/// A writer that hashes and counts exactly the bytes its inner writer accepted.
struct HashingWriter<Inner> {
    inner: Inner,
    hasher: Sha256,
    bytes: u64,
}

impl<Inner: Write> Write for HashingWriter<Inner> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        // A short write hashes only what was accepted; the buffer retries the rest.
        self.hasher.update(&buf[..written]);
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The sidecar's path relative to the scratch.
pub(crate) fn sidecar_file(phase_index: usize, phase_name: &str) -> String {
    format!("{SIDECAR_DIR}/{phase_index}-{phase_name}.json")
}

/// Stream the sidecar into `inner` through one 64 KiB buffer, hashing exactly the bytes `inner` accepted; no
/// whole-document buffer and no copy of the records exist. Returns `inner` with the size and digest.
fn stream_to<Inner: Write, Records: TakenRecords>(
    inner: Inner,
    binding: &Binding<'_>,
    records: &Records,
) -> io::Result<(Inner, u64, String)> {
    let hashing = HashingWriter { inner, hasher: Sha256::new(), bytes: 0 };
    let mut buffered = BufWriter::with_capacity(WRITE_BUFFER_BYTES, hashing);
    serde_json::to_writer(&mut buffered, &Sidecar { binding, records })
        .map_err(io::Error::other)?;
    let hashing = buffered.into_inner().map_err(|error| error.into_error())?;
    Ok((hashing.inner, hashing.bytes, hashing.hasher.finish_hex()))
}

/// Stream the sidecar to `temporary`, then fsync it; returns its size and digest.
fn stream_sidecar<Records: TakenRecords>(
    temporary: &Path,
    binding: &Binding<'_>,
    records: &Records,
) -> io::Result<(u64, String)> {
    let (file, bytes, sha256) = stream_to(std::fs::File::create(temporary)?, binding, records)?;
    file.sync_all()?;
    Ok((bytes, sha256))
}

/// Write `records` as the phase's sidecar under `scratch`: to `.tmp`, fsync, then `rename` into place. Any
/// failure removes the temporary file and gives `write_failed`, so a phase never names a partial file.
fn write_sidecar<Records: TakenRecords>(
    scratch: &Path,
    binding: &Binding<'_>,
    records: &Records,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> GuardCorrelationField {
    let window = WindowRecord::from(binding.window);
    let file = sidecar_file(binding.phase_index, binding.phase_name);
    let target = scratch.join(&file);
    let mut temporary = target.clone().into_os_string();
    temporary.push(".tmp");
    let temporary = std::path::PathBuf::from(temporary);
    let written = std::fs::create_dir_all(scratch.join(SIDECAR_DIR))
        .and_then(|()| stream_sidecar(&temporary, binding, records))
        .and_then(|digest| rename(&temporary, &target).map(|()| digest));
    match written {
        Ok((bytes, sha256)) => GuardCorrelationField {
            status: TransportStatus::Written,
            file: Some(file),
            take_seq: Some(records.take_seq()),
            bytes: Some(bytes),
            sha256: Some(sha256),
            window,
        },
        Err(error) => {
            // When: any step failed, the partial file is removed (a missing one is ignored) and none is named.
            let _ = std::fs::remove_file(&temporary);
            tracing::warn!(target: "sonicterm::perf_scenarios", file, %error, "guard-correlation sidecar not written");
            GuardCorrelationField {
                take_seq: Some(records.take_seq()),
                ..GuardCorrelationField::status_only(TransportStatus::WriteFailed, window)
            }
        }
    }
}

/// One phase's transport of `outcome`: a status and the window for every outcome, and the sidecar for a take.
pub(crate) fn transport<Records: TakenRecords>(
    scratch: &Path,
    binding: &Binding<'_>,
    outcome: &GuardTake<Records>,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> GuardCorrelationField {
    let window = WindowRecord::from(binding.window);
    match outcome {
        GuardTake::Unavailable => {
            GuardCorrelationField::status_only(TransportStatus::Unavailable, window)
        }
        GuardTake::GateOff => GuardCorrelationField::status_only(TransportStatus::GateOff, window),
        GuardTake::TakeExhausted => {
            GuardCorrelationField::status_only(TransportStatus::TakeExhausted, window)
        }
        GuardTake::Taken(records) => write_sidecar(scratch, binding, records, rename),
    }
}

/// The App's take, the shared clock epoch and borrowed serializers over the App's transferred buffers, all
/// only in a build with the cfg.
#[cfg(perf_guard_spans_api)]
pub(crate) mod api {
    use std::time::Instant;

    use serde::ser::{SerializeSeq, SerializeStruct, Serializer};
    use sonicterm_app::app::{
        clock_epoch, AbandonedSectionV1, App, GuardCorrelationTakeV1, GuardCorrelationV1,
        LossIntervalV1, PaneSectionsV1, PendingSectionV1, SectionRecordV1, SpanBatchV1,
        SpanRecordV1,
    };

    use super::{GuardTake, TakenRecords};

    /// The records one take carries in this build: the App's own transfer, borrowed when serialized.
    pub(crate) type Records = GuardCorrelationV1;

    /// Set the shared clock epoch, before the startup meter begins, so startup's start converts.
    pub(crate) fn bootstrap_epoch() {
        let _ = clock_epoch();
    }

    /// Nanoseconds from the shared epoch to `at`; `None` before the epoch or past `u64`.
    pub(crate) fn epoch_ns(at: Instant) -> Option<u64> {
        let elapsed = at.checked_duration_since(clock_epoch())?;
        u64::try_from(elapsed.as_nanos()).ok()
    }

    /// Take the App's records; every outcome is named, so a new one fails the build.
    pub(crate) fn take(app: &mut App) -> GuardTake<Records> {
        match app.take_guard_correlation_v1() {
            GuardCorrelationTakeV1::GateOff => GuardTake::GateOff,
            GuardCorrelationTakeV1::TakeExhausted => GuardTake::TakeExhausted,
            GuardCorrelationTakeV1::Taken(taken) => GuardTake::Taken(taken),
        }
    }

    /// A borrowed slice written as a JSON array, one adapter per element.
    struct Each<'records, Item, Adapter>(&'records [Item], fn(&'records Item) -> Adapter);

    impl<'records, Item, Adapter: serde::Serialize> serde::Serialize for Each<'records, Item, Adapter> {
        fn serialize<Out: Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
            let mut seq = out.serialize_seq(Some(self.0.len()))?;
            for item in self.0 {
                seq.serialize_element(&(self.1)(item))?;
            }
            seq.end()
        }
    }

    /// One published section, by reference.
    struct SectionOut<'records>(&'records SectionRecordV1);

    impl serde::Serialize for SectionOut<'_> {
        fn serialize<Out: Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
            let mut record = out.serialize_struct("Section", 3)?;
            record.serialize_field("section_seq", &self.0.section_seq)?;
            record.serialize_field("before_lock_ns", &self.0.before_lock_ns)?;
            record.serialize_field("locked_at_ns", &self.0.locked_at_ns)?;
            record.end()
        }
    }

    /// One abandoned section, by reference.
    struct AbandonedOut<'records>(&'records AbandonedSectionV1);

    impl serde::Serialize for AbandonedOut<'_> {
        fn serialize<Out: Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
            let mut entry = out.serialize_struct("Abandoned", 3)?;
            entry.serialize_field("section_seq", &self.0.section_seq)?;
            entry.serialize_field("registered_ns", &self.0.registered_ns)?;
            entry.serialize_field("abandoned_ns", &self.0.abandoned_ns)?;
            entry.end()
        }
    }

    /// One loss interval, by reference.
    struct LossOut<'records>(&'records LossIntervalV1);

    impl serde::Serialize for LossOut<'_> {
        fn serialize<Out: Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
            let mut loss = out.serialize_struct("Loss", 2)?;
            loss.serialize_field("from_ns", &self.0.from_ns)?;
            loss.serialize_field("to_ns", &self.0.to_ns)?;
            loss.end()
        }
    }

    /// The pending section, `null` when there is none.
    struct PendingOut(Option<PendingSectionV1>);

    impl serde::Serialize for PendingOut {
        fn serialize<Out: Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
            let Some(pending) = self.0 else {
                // When: the log has no pending section, the field is written as null.
                return out.serialize_none();
            };
            let mut entry = out.serialize_struct("Pending", 2)?;
            entry.serialize_field("section_seq", &pending.section_seq)?;
            entry.serialize_field("registered_ns", &pending.registered_ns)?;
            entry.end()
        }
    }

    /// One UI span, by reference.
    struct SpanOut<'records>(&'records SpanRecordV1);

    impl serde::Serialize for SpanOut<'_> {
        fn serialize<Out: Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
            let mut span = out.serialize_struct("Span", 4)?;
            span.serialize_field("pane_id", &self.0.pane_id)?;
            span.serialize_field("collection_seq", &self.0.collection_seq)?;
            span.serialize_field("acquired_ns", &self.0.acquired_ns)?;
            span.serialize_field("released_ns", &self.0.released_ns)?;
            span.end()
        }
    }

    /// One pane transfer, by reference, every field by name.
    struct PaneOut<'records>(&'records PaneSectionsV1);

    impl serde::Serialize for PaneOut<'_> {
        fn serialize<Out: Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
            let pane = self.0;
            let mut fields = out.serialize_struct("Pane", 20)?;
            fields.serialize_field("pane_id", &pane.pane_id)?;
            fields.serialize_field("closed", &pane.closed)?;
            fields.serialize_field("identities_exhausted", &pane.identities_exhausted)?;
            fields.serialize_field("prev_next_section_seq", &pane.prev_next_section_seq)?;
            fields.serialize_field("next_section_seq", &pane.next_section_seq)?;
            fields.serialize_field("carried_pending", &pane.carried_pending)?;
            fields.serialize_field("records", &Each(&pane.records, SectionOut))?;
            fields.serialize_field("pending", &PendingOut(pane.pending))?;
            fields.serialize_field("abandoned", &Each(&pane.abandoned, AbandonedOut))?;
            fields.serialize_field("abandoned_overflow", &pane.abandoned_overflow)?;
            fields.serialize_field("abandoned_unlocated", &pane.abandoned_unlocated)?;
            fields.serialize_field("issued_dropped_located", &pane.issued_dropped_located)?;
            fields.serialize_field("issued_dropped_unlocated", &pane.issued_dropped_unlocated)?;
            fields.serialize_field("losses", &Each(&pane.losses, LossOut))?;
            fields.serialize_field("loss_events", &pane.loss_events)?;
            fields.serialize_field("losses_merged", &pane.losses_merged)?;
            fields.serialize_field("refused_closed", &pane.refused_closed)?;
            fields.serialize_field("refused_exhausted", &pane.refused_exhausted)?;
            fields.serialize_field("refused_unlocated", &pane.refused_unlocated)?;
            fields.serialize_field("first_refusal_ns", &pane.first_refusal_ns)?;
            fields.end()
        }
    }

    /// The span batch, by reference, every field by name.
    struct BatchOut<'records>(&'records SpanBatchV1);

    impl serde::Serialize for BatchOut<'_> {
        fn serialize<Out: Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
            let batch = self.0;
            let mut fields = out.serialize_struct("SpanBatch", 13)?;
            fields.serialize_field("spans", &Each(&batch.spans, SpanOut))?;
            fields.serialize_field("spans_issued", &batch.spans_issued)?;
            fields.serialize_field("spans_dropped_located", &batch.spans_dropped_located)?;
            fields.serialize_field("spans_dropped_unlocated", &batch.spans_dropped_unlocated)?;
            fields.serialize_field("losses", &Each(&batch.losses, LossOut))?;
            fields.serialize_field("loss_events", &batch.loss_events)?;
            fields.serialize_field("losses_merged", &batch.losses_merged)?;
            fields.serialize_field("collections_issued", &batch.collections_issued)?;
            fields.serialize_field(
                "collections_refused_exhausted",
                &batch.collections_refused_exhausted,
            )?;
            fields.serialize_field("prev_next_collection_seq", &batch.prev_next_collection_seq)?;
            fields.serialize_field("next_collection_seq", &batch.next_collection_seq)?;
            fields.serialize_field("identities_exhausted", &batch.identities_exhausted)?;
            fields.serialize_field("open_collections", &batch.open_collections)?;
            fields.end()
        }
    }

    impl TakenRecords for GuardCorrelationV1 {
        fn take_seq(&self) -> u64 {
            self.take_seq
        }

        fn taken_at_ns(&self) -> Option<u64> {
            self.taken_at_ns
        }

        fn serialize_panes<Out: Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
            Each(&self.panes, PaneOut).serialize(out)
        }

        fn serialize_spans<Out: Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
            serde::Serialize::serialize(&BatchOut(&self.spans), out)
        }
    }

    use serde::Serialize as _;
}

/// Without the cfg the App is never called: no epoch is set, no time converts, and every take is unavailable.
#[cfg(not(perf_guard_spans_api))]
pub(crate) mod api {
    use std::time::Instant;

    use sonicterm_app::app::App;

    use super::{GuardTake, TakenRecords};

    /// This build never names the App's records, so no value of them exists.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Records {}

    impl TakenRecords for Records {
        fn take_seq(&self) -> u64 {
            match *self {}
        }

        fn taken_at_ns(&self) -> Option<u64> {
            match *self {}
        }

        fn serialize_panes<Out: serde::Serializer>(
            &self,
            _out: Out,
        ) -> Result<Out::Ok, Out::Error> {
            match *self {}
        }

        fn serialize_spans<Out: serde::Serializer>(
            &self,
            _out: Out,
        ) -> Result<Out::Ok, Out::Error> {
            match *self {}
        }
    }

    /// Nothing to set in this build.
    pub(crate) fn bootstrap_epoch() {}

    /// No epoch is named in this build.
    pub(crate) fn epoch_ns(_at: Instant) -> Option<u64> {
        None
    }

    /// The App is never called in this build.
    pub(crate) fn take(_app: &mut App) -> GuardTake<Records> {
        GuardTake::Unavailable
    }
}

#[cfg(test)]
#[path = "guard_transport_tests.rs"]
mod guard_transport_tests;
