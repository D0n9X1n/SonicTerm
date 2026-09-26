use super::*;

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::atomic::AtomicUsize;

fn validation(description: &str) -> wgpu::Error {
    wgpu::Error::Validation {
        source: Box::new(std::io::Error::other(description.to_owned())),
        description: description.to_owned(),
    }
}

fn out_of_memory() -> wgpu::Error {
    wgpu::Error::OutOfMemory { source: Box::new(std::io::Error::other("out of memory")) }
}

fn internal(description: &str) -> wgpu::Error {
    wgpu::Error::Internal {
        source: Box::new(std::io::Error::other(description.to_owned())),
        description: description.to_owned(),
    }
}

fn counting_waker(state: &DeviceErrorState) -> Arc<AtomicUsize> {
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    assert!(state.set_waker(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })));
    wakes
}

/// States only move forward: an error after a loss keeps the device lost, and
/// the first stop stays the recorded cause.
#[test]
fn transitions_are_one_way() {
    let state = DeviceErrorState::new();
    assert_eq!(state.state(), DeviceState::Usable);
    assert!(state.accepts_gpu_work());

    state.record_uncaptured(&validation("first"));
    assert_eq!(state.state(), DeviceState::Unusable);
    assert!(!state.accepts_gpu_work());

    state.record_lost(wgpu::DeviceLostReason::Unknown, "driver reset");
    assert_eq!(state.state(), DeviceState::Lost);

    state.record_uncaptured(&validation("after loss"));
    let snapshot = state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Lost);
    assert_eq!(snapshot.unusable.map(|record| record.description), Some("first".to_owned()));
    assert_eq!(snapshot.counts.validation, 2);
    assert_eq!(snapshot.records_logged, 2);
}

/// A loss while usable records `Lost` directly, with no `Unusable` record.
#[test]
fn loss_from_usable_goes_straight_to_lost() {
    let state = DeviceErrorState::new();
    state.record_lost(wgpu::DeviceLostReason::Destroyed, "");
    let snapshot = state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Lost);
    assert!(snapshot.unusable.is_none());
    assert_eq!(snapshot.lost.map(|record| record.description), Some("Destroyed".to_owned()));
}

/// The lost record is set once: a second callback only raises the count.
#[test]
fn lost_record_is_set_once() {
    let state = DeviceErrorState::new();
    state.record_lost(wgpu::DeviceLostReason::Unknown, "first reset");
    state.record_lost(wgpu::DeviceLostReason::Destroyed, "second");
    let snapshot = state.snapshot();
    let lost = snapshot.lost.expect("lost record");
    assert_eq!(lost.kind, DeviceErrorKind::Lost);
    assert_eq!(lost.description, "Unknown: first reset");
    assert_eq!(snapshot.counts.lost, 2);
    assert_eq!(snapshot.records_logged, 1);
}

/// The app is woken once per transition, not once per error; repeats and
/// isolated errors never wake it.
#[test]
fn one_wake_per_transition() {
    let state = DeviceErrorState::new();
    let wakes = counting_waker(&state);
    state.record_isolated(&validation("isolated"));
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
    for _ in 0..3 {
        state.record_uncaptured(&validation("repeat"));
    }
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    state.record_lost(wgpu::DeviceLostReason::Unknown, "reset");
    state.record_lost(wgpu::DeviceLostReason::Unknown, "reset again");
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    assert_eq!(state.snapshot().wakes, 2);
}

/// Renderers sharing a device keep the first waker; a second one is refused
/// and never runs.
#[test]
fn first_waker_wins() {
    let state = DeviceErrorState::new();
    let wakes = counting_waker(&state);
    let refused = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&refused);
    assert!(!state.set_waker(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })));
    state.record_uncaptured(&validation("stop"));
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert_eq!(refused.load(Ordering::SeqCst), 0);
}

/// Without an installed waker a transition still records and logs, and counts
/// no wake.
#[test]
fn transition_without_waker_counts_no_wake() {
    let state = DeviceErrorState::new();
    state.record_uncaptured(&out_of_memory());
    let snapshot = state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Unusable);
    assert_eq!(snapshot.wakes, 0);
    assert_eq!(snapshot.records_logged, 1);
    assert_eq!(snapshot.unusable.map(|record| record.kind), Some(DeviceErrorKind::OutOfMemory));
}

/// Each kind has its own coalesced counter; only the stop and the first
/// isolated error produce records.
#[test]
fn counters_coalesce_by_kind() {
    let state = DeviceErrorState::new();
    state.record_uncaptured(&validation("v"));
    state.record_uncaptured(&validation("v"));
    state.record_uncaptured(&out_of_memory());
    state.record_uncaptured(&internal("i"));
    state.record_isolated(&validation("iso"));
    state.record_isolated(&validation("iso"));
    let snapshot = state.snapshot();
    assert_eq!(
        snapshot.counts,
        DeviceErrorCounts { validation: 2, out_of_memory: 1, internal: 1, isolated: 2, lost: 0 }
    );
    assert_eq!(snapshot.records_logged, 2);
    assert_eq!(snapshot.unusable.map(|record| record.kind), Some(DeviceErrorKind::Validation));
    assert_eq!(snapshot.isolated.map(|record| record.kind), Some(DeviceErrorKind::Isolated));
}

/// A validation stop the renderer observed itself counts only when the handler
/// has not already stopped the device, so one error is never counted twice.
#[test]
fn observed_validation_is_not_double_counted() {
    let fresh = DeviceErrorState::new();
    fresh.record_observed_validation("surface acquisition validation");
    assert_eq!(fresh.state(), DeviceState::Unusable);
    assert_eq!(fresh.counts().validation, 1);

    let stopped = DeviceErrorState::new();
    stopped.record_uncaptured(&validation("handler saw it"));
    stopped.record_observed_validation("surface acquisition validation");
    assert_eq!(stopped.counts().validation, 1);
    assert_eq!(
        stopped.snapshot().unusable.map(|record| record.description),
        Some("handler saw it".to_owned())
    );
}

fn every_gate() -> Vec<DeviceGate> {
    let mut gates = Vec::new();
    for state in [DeviceState::Usable, DeviceState::Unusable, DeviceState::Lost] {
        for destroy_requested in [false, true] {
            gates.push(DeviceGate { state, destroy_requested });
        }
    }
    gates
}

/// Over every combination of the three readings, the first checkpoint that
/// refused work decides the outcome, and only `Presented` acknowledges.
#[test]
fn decision_table_covers_every_reading() {
    let gates = every_gate();
    for &before in &gates {
        for &after_submit in &gates {
            for &after_present in &gates {
                let outcome = decide_frame_outcome(before, after_submit, after_present);
                let expected = if !before.accepts_gpu_work() {
                    DeviceFrameOutcome::NotStarted
                } else if !after_submit.accepts_gpu_work() {
                    DeviceFrameOutcome::SubmittedNotPresented
                } else if !after_present.accepts_gpu_work() {
                    DeviceFrameOutcome::PresentedNotAcknowledged
                } else {
                    DeviceFrameOutcome::Presented
                };
                assert_eq!(outcome, expected, "{before:?} {after_submit:?} {after_present:?}");
                assert_eq!(
                    outcome.acknowledges(),
                    before.accepts_gpu_work()
                        && after_submit.accepts_gpu_work()
                        && after_present.accepts_gpu_work()
                );
            }
        }
    }
}

/// Named rows: a pending destroy refuses work like a stop, a failed submission
/// never presents, and a failed presentation is never acknowledged.
#[test]
fn decision_table_named_rows() {
    let usable = DeviceGate { state: DeviceState::Usable, destroy_requested: false };
    let unusable = DeviceGate { state: DeviceState::Unusable, destroy_requested: false };
    let lost = DeviceGate { state: DeviceState::Lost, destroy_requested: false };
    let destroying = DeviceGate { state: DeviceState::Usable, destroy_requested: true };
    let decide = decide_frame_outcome;
    assert_eq!(decide(usable, usable, usable), DeviceFrameOutcome::Presented);
    assert_eq!(decide(unusable, usable, usable), DeviceFrameOutcome::NotStarted);
    assert_eq!(decide(destroying, usable, usable), DeviceFrameOutcome::NotStarted);
    assert_eq!(decide(usable, unusable, unusable), DeviceFrameOutcome::SubmittedNotPresented);
    assert_eq!(decide(usable, lost, lost), DeviceFrameOutcome::SubmittedNotPresented);
    assert_eq!(decide(usable, usable, unusable), DeviceFrameOutcome::PresentedNotAcknowledged);
    assert_eq!(decide(usable, usable, destroying), DeviceFrameOutcome::PresentedNotAcknowledged);
    assert!(!DeviceFrameOutcome::NotStarted.presents());
    assert!(!DeviceFrameOutcome::SubmittedNotPresented.presents());
    assert!(DeviceFrameOutcome::PresentedNotAcknowledged.presents());
    assert!(!DeviceFrameOutcome::PresentedNotAcknowledged.acknowledges());
    assert!(DeviceFrameOutcome::Presented.acknowledges());
}

/// The gate admits work only while usable and not destroy-requested, a refused
/// scope never runs its work, and both outcomes are counted.
#[test]
fn gate_refuses_work_after_stop_or_destroy_request() {
    let destroying = DeviceErrorState::new();
    assert_eq!(destroying.gpu_work("probe", || 7), Some(7));
    destroying.request_destroy();
    let mut ran = false;
    assert_eq!(destroying.gpu_work("probe", || ran = true), None);
    assert!(!ran);
    assert!(destroying.enter_gpu_work("probe").is_none());
    let snapshot = destroying.snapshot();
    assert!(snapshot.destroy_requested);
    assert_eq!(snapshot.state, DeviceState::Usable);
    assert_eq!((snapshot.admitted_work, snapshot.refused_work), (1, 2));

    let stopped = DeviceErrorState::new();
    stopped.record_uncaptured(&internal("stop"));
    assert!(stopped.enter_gpu_work("probe").is_none());
    assert_eq!(stopped.snapshot().refused_work, 1);
}

/// Errors carry the innermost live label, a relabel covers only later errors,
/// and dropping a scope restores the enclosing label.
#[test]
fn scopes_label_errors_and_restore_on_drop() {
    assert_eq!(current_operation(), UNLABELLED_OPERATION);
    let state = DeviceErrorState::new();
    let outer = state.enter_gpu_work("outer").expect("usable device");
    {
        let inner = state.enter_gpu_work("inner").expect("usable device");
        assert_eq!(current_operation(), "inner");
        inner.set_operation("inner.relabelled");
        state.record_uncaptured(&validation("labelled"));
    }
    assert_eq!(current_operation(), "outer");
    drop(outer);
    assert_eq!(current_operation(), UNLABELLED_OPERATION);
    assert_eq!(state.snapshot().unusable.map(|record| record.operation), Some("inner.relabelled"));
}

/// Each state gets its own device generation, so renderers can tell whether
/// they share a device.
#[test]
fn generations_are_unique() {
    let first = DeviceErrorState::new();
    let second = DeviceErrorState::new();
    assert_ne!(first.generation(), second.generation());
    assert_eq!(first.snapshot().generation, first.generation());
}

/// Renderer methods that must issue their GPU work through the device gate.
const GATED_METHODS: &[&str] = &[
    "new_async",
    "try_resize",
    "set_software_render_degrade",
    "rebuild_glyph_upload_if_needed",
    "rebuild_image_upload_if_needed",
    "allocator_snapshot",
    "render_frame",
    "present_software_frame",
    "present_wgpu_frame",
    "__inject_gpu_fault",
    "prepare_rebind",
    "commit_rebind",
];

/// Renderer methods that reach the device without issuing GPU work: they
/// clone its handles, compare their identity, or read its features.
const NOT_GPU_WORK: &[&str] = &[
    "shared_context",
    // Compares the instance and device handles for identity; it issues no GPU work.
    "shares_device_with",
    "effective_subpixel_aa_mode",
    "log_subpixel_aa_policy",
    "set_subpixel_aa_mode",
    "set_theme",
    "set_theme_with_opacity",
];

/// Receivers and fields that hold a wgpu handle: the renderer's own, a candidate
/// context's, and a candidate surface's.
const GPU_HANDLES: &[&str] = &[
    "self.device",
    "self.queue",
    "self.surface",
    "self.instance",
    "context.device",
    "context.queue",
    "context.instance",
    "surface.surface",
    "surface.instance",
];

/// Device calls that only return values fixed when the device was created, so
/// they issue no GPU work and cannot raise an error.
const DEVICE_READS: &[&str] = &["self.device.limits()", "self.device.features()"];

/// The receivers the gate is called on: the renderer's own state, and the
/// state `new_async` binds before the renderer exists.
const GATE_RECEIVERS: &[&str] = &["self.device_errors", "errors"];

/// Builds the device. Only its explicitly named pre-device bootstrap calls
/// are exempt; surface configuration and retained-resource creation are gated.
const CONSTRUCTOR: &str = "new_async";

/// GPU work that bypasses the gate on purpose, with the method it belongs to:
/// the destroy fault must reach a device that an earlier fault already stopped.
const UNGATED_BY_DESIGN: &[(&str, &str)] =
    &[("__inject_gpu_fault", "destroy_and_await_loss(&self.device, &self.device_errors)")];

/// The exact read-only operations the recovery surface helper may perform without a
/// gate: reading the lost surface's Metal layer and wrapping it for a new instance.
/// Only these spellings are masked; any other handle use, `wgpu::` path, or call into
/// a GPU-reaching method in that helper still counts.
const SURFACE_READS: &[(&str, &str)] = &[
    ("candidate_surface", "self.surface.as_hal::<wgpu::hal::api::Metal>()"),
    ("candidate_surface", "wgpu::SurfaceTargetUnsafe::CoreAnimationLayer("),
];

/// `code` with the exact read-only surface operations of `method` blanked, keeping
/// every offset.
fn mask_surface_reads(method: &str, code: &str) -> String {
    let mut masked = code.to_owned();
    for (owner, snippet) in SURFACE_READS {
        if *owner == method {
            masked = masked.replace(snippet, &" ".repeat(snippet.len()));
        }
    }
    masked
}

/// Calls that wgpu treats as fatal whatever handler is installed, or whose
/// errors panic in a helper, plus error scopes, which would hide errors from
/// the handler. Method and path spellings are both listed.
const BANNED: &[&str] = &[
    ".poll(",
    "::poll(",
    ".poll_all(",
    "::poll_all(",
    "poll_all_devices",
    "create_render_bundle_encoder",
    "create_buffer_init",
    "create_texture_with_data",
    "DeviceExt",
    "get_mapped_range",
    "map_async",
    "mapped_at_creation: true",
    "push_error_scope",
];

fn is_ident_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let start = from.min(haystack.len());
    haystack[start..].windows(needle.len()).position(|window| window == needle).map(|at| start + at)
}

/// Replace every byte in `range` except line breaks with a space.
fn blank(bytes: &mut [u8], range: Range<usize>) {
    let end = range.end.min(bytes.len());
    for byte in &mut bytes[range.start.min(end)..end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

fn block_comment_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    let mut at = start;
    while at + 1 < bytes.len() {
        if bytes[at] == b'/' && bytes[at + 1] == b'*' {
            depth += 1;
            at += 2;
        } else if bytes[at] == b'*' && bytes[at + 1] == b'/' {
            depth -= 1;
            at += 2;
            if depth == 0 {
                return at;
            }
        } else {
            at += 1;
        }
    }
    bytes.len()
}

/// The index of the quote that closes the string opened at `open`.
fn string_end(bytes: &[u8], open: usize) -> usize {
    let mut at = open + 1;
    while at < bytes.len() && bytes[at] != b'"' {
        at += if bytes[at] == b'\\' { 2 } else { 1 };
    }
    at.min(bytes.len())
}

/// The end of a raw string literal that starts at `at`, if one does.
fn raw_string_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes[at] != b'r' {
        return None;
    }
    let previous = |offset: usize| at.checked_sub(offset).map(|index| bytes[index]);
    let prefixed = match previous(1) {
        Some(b'b') => !previous(2).is_some_and(is_ident_byte),
        Some(byte) => !is_ident_byte(byte),
        None => true,
    };
    if !prefixed {
        return None;
    }
    let mut quote = at + 1;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    let mut terminator = vec![b'"'];
    terminator.resize(quote - at, b'#');
    find_bytes(bytes, &terminator, quote + 1).map(|end| end + terminator.len())
}

/// The end of a character literal that starts at `at`, if one does; a
/// lifetime or a label is not one.
fn char_literal_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes[at] != b'\'' {
        return None;
    }
    let first = *bytes.get(at + 1)?;
    if first == b'\\' {
        return find_bytes(bytes, b"'", at + 3).map(|end| end + 1);
    }
    let width = match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    };
    (bytes.get(at + 1 + width) == Some(&b'\'')).then_some(at + 2 + width)
}

/// Normalize checkout line endings, then blank comments and literals while
/// preserving offsets within that normalized source.
fn code_only(source: &str) -> String {
    let normalized = source.replace("\r\n", "\n");
    let mut bytes = normalized.as_bytes().to_vec();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        let next = bytes.get(at + 1).copied();
        if byte == b'/' && next == Some(b'/') {
            let end = find_bytes(&bytes, b"\n", at).unwrap_or(bytes.len());
            blank(&mut bytes, at..end);
            at = end;
        } else if byte == b'/' && next == Some(b'*') {
            let end = block_comment_end(&bytes, at);
            blank(&mut bytes, at..end);
            at = end;
        } else if let Some(end) = raw_string_end(&bytes, at) {
            blank(&mut bytes, at..end);
            at = end;
        } else if byte == b'"' {
            let end = string_end(&bytes, at);
            blank(&mut bytes, at + 1..end);
            at = end + 1;
        } else if let Some(end) = char_literal_end(&bytes, at) {
            blank(&mut bytes, at + 1..end - 1);
            at = end;
        } else {
            at += 1;
        }
    }
    String::from_utf8(bytes).expect("blanking keeps UTF-8 boundaries")
}

struct Method {
    name: String,
    code: String,
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// The indentation of a line that opens an inherent `impl GpuRenderer` block.
fn renderer_impl_indent(line: &str) -> Option<usize> {
    let head = line.trim().strip_prefix("impl ")?.strip_suffix(" {")?;
    if head == "GpuRenderer" || head.ends_with("::GpuRenderer") {
        Some(indent_of(line))
    } else {
        None
    }
}

fn method_name(line: &str, indent: usize) -> Option<&str> {
    if indent_of(line) != indent {
        return None;
    }
    let (head, after_fn) = line[indent..].split_once("fn ")?;
    let modifiers = head
        .split_whitespace()
        .all(|word| word.starts_with("pub") || matches!(word, "const" | "async" | "unsafe"));
    if !modifiers {
        return None;
    }
    let end = after_fn.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    Some(&after_fn[..end])
}

/// Join rustfmt's split method chains, so a receiver and its call read as one
/// expression.
fn compact_chains(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut pending = String::new();
    for character in code.chars() {
        if character.is_whitespace() {
            pending.push(character);
            continue;
        }
        if character != '.' {
            out.push_str(&pending);
        }
        pending.clear();
        out.push(character);
    }
    out.push_str(&pending);
    out
}

/// Every method of every `impl GpuRenderer` block in the blanked `sources`,
/// with cfg-gated duplicates merged, and the number of blocks found.
fn renderer_methods(sources: &[String]) -> (usize, Vec<Method>) {
    let mut blocks = 0;
    let mut methods: Vec<Method> = Vec::new();
    for source in sources {
        let lines: Vec<&str> = source.lines().collect();
        let mut index = 0;
        while index < lines.len() {
            let Some(indent) = renderer_impl_indent(lines[index]) else {
                index += 1;
                continue;
            };
            blocks += 1;
            let close = format!("{}}}", " ".repeat(indent));
            let mut current: Option<usize> = None;
            index += 1;
            while index < lines.len() && lines[index] != close {
                let line = lines[index];
                if let Some(name) = method_name(line, indent + 4) {
                    let existing = methods.iter().position(|method| method.name == name);
                    current = Some(existing.unwrap_or_else(|| {
                        methods.push(Method { name: name.to_owned(), code: String::new() });
                        methods.len() - 1
                    }));
                }
                if let Some(current) = current {
                    methods[current].code.push_str(line);
                    methods[current].code.push('\n');
                }
                index += 1;
            }
        }
    }
    for method in &mut methods {
        method.code = compact_chains(&method.code);
    }
    (blocks, methods)
}

/// The index of the bracket that closes the one at `open`.
fn matching_close(code: &[u8], open: usize) -> usize {
    let (opening, closing) = match code[open] {
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        _ => (b'{', b'}'),
    };
    let mut depth = 0usize;
    for (index, &byte) in code.iter().enumerate().skip(open) {
        if byte == opening {
            depth += 1;
        } else if byte == closing {
            depth -= 1;
            if depth == 0 {
                return index;
            }
        }
    }
    code.len()
}

/// The index of the brace that closes the block enclosing `from`.
fn enclosing_close(code: &[u8], from: usize) -> usize {
    let mut depth = 0usize;
    for (index, &byte) in code.iter().enumerate().skip(from) {
        if byte == b'{' {
            depth += 1;
        } else if byte == b'}' {
            if depth == 0 {
                return index;
            }
            depth -= 1;
        }
    }
    code.len()
}

fn skip_whitespace(code: &[u8], mut at: usize) -> usize {
    while code.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// How a gate call's result is bound, which decides what code it controls.
enum GateBinding {
    /// `if let Some(scope) = gate { work }`: the work is the body.
    IfLet,
    /// `let Some(scope) = gate else { return };`: the work follows it.
    LetElse,
    /// `let scope = gate.ok_or_else(..)?;`: the work follows it.
    Propagated,
}

/// `text` without a trailing whole word `word`.
fn strip_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = text.trim_end().strip_suffix(word)?;
    if rest.as_bytes().last().is_some_and(|&byte| is_ident_byte(byte)) {
        return None;
    }
    Some(rest)
}

/// The binding of a gate call, read from the text before the call.
fn gate_binding(before: &str) -> Option<GateBinding> {
    let before = before.trim_end().strip_suffix('=')?;
    if before.ends_with(['=', '!', '<', '>']) {
        return None;
    }
    let before = before.trim_end();
    if let Some(pattern) = before.strip_suffix(')') {
        let open = pattern.rfind('(')?;
        let binding = strip_word(&pattern[..open], "Some")?;
        let keyword = strip_word(binding, "let")?;
        return Some(if strip_word(keyword, "if").is_some() {
            GateBinding::IfLet
        } else {
            GateBinding::LetElse
        });
    }
    let name = before.trim_end_matches(|c: char| c == '_' || c.is_ascii_alphanumeric());
    if name.len() == before.len() {
        return None;
    }
    let name = strip_word(name, "mut").unwrap_or(name);
    strip_word(name, "let").map(|_| GateBinding::Propagated)
}

/// Byte ranges of `code` that run only after the device gate admitted work: a
/// `gpu_work` closure, an `if let` body, or the rest of the block after a
/// `let`-`else` or propagated scope. A gate call whose result is discarded or
/// only tested controls nothing.
fn gate_ranges(code: &str) -> Vec<Range<usize>> {
    let bytes = code.as_bytes();
    let mut ranges = Vec::new();
    for receiver in GATE_RECEIVERS {
        for call in [".gpu_work(", ".enter_gpu_work("] {
            let pattern = format!("{receiver}{call}");
            for (at, _) in code.match_indices(pattern.as_str()) {
                if at > 0 && (is_ident_byte(bytes[at - 1]) || bytes[at - 1] == b'.') {
                    continue;
                }
                let open = at + pattern.len() - 1;
                let close = matching_close(bytes, open);
                if call == ".gpu_work(" {
                    if let Some(body) = gpu_work_closure_body(code, open, close) {
                        ranges.push(body);
                    }
                    continue;
                }
                let after = skip_whitespace(bytes, close + 1);
                let rest = code.get(after..).unwrap_or_default();
                match gate_binding(&code[..at]) {
                    Some(GateBinding::IfLet) if rest.starts_with('{') => {
                        ranges.push(after..matching_close(bytes, after));
                    }
                    Some(GateBinding::LetElse) if rest.starts_with("else") => {
                        let brace = skip_whitespace(bytes, after + 4);
                        if bytes.get(brace) == Some(&b'{') {
                            let end = matching_close(bytes, brace) + 1;
                            ranges.push(end..enclosing_close(bytes, end));
                        }
                    }
                    Some(GateBinding::Propagated) => {
                        for conversion in [".ok_or_else(", ".ok_or("] {
                            if !rest.starts_with(conversion) {
                                continue;
                            }
                            let converted = matching_close(bytes, after + conversion.len() - 1);
                            let question = skip_whitespace(bytes, converted + 1);
                            if bytes.get(question) == Some(&b'?') {
                                ranges.push(question + 1..enclosing_close(bytes, question + 1));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    ranges
}

/// Recognize the production zero-argument closure after the label argument.
/// Label expressions are evaluated before the gate, so none of that argument
/// may be counted as guarded. Skip only balanced argument delimiters here.
fn gpu_work_closure_body(code: &str, open: usize, close: usize) -> Option<Range<usize>> {
    let bytes = code.as_bytes();
    let mut at = open + 1;
    while at < close {
        match bytes[at] {
            b'(' | b'[' | b'{' => at = matching_close(bytes, at) + 1,
            b',' => break,
            _ => at += 1,
        }
    }
    if at >= close {
        return None;
    }
    at = skip_whitespace(bytes, at + 1);
    if code[at..].starts_with("move ") {
        at = skip_whitespace(bytes, at + 4);
    }
    if !code[at..].starts_with("||") {
        return None;
    }
    let body = skip_whitespace(bytes, at + 2);
    let end = if bytes.get(body) == Some(&b'{') { matching_close(bytes, body) + 1 } else { close };
    (body < end && end <= close).then_some(body..end)
}

fn is_whole_field(code: &[u8], at: usize, len: usize) -> bool {
    let before = at.checked_sub(1).map(|index| code[index]);
    let after = code.get(at + len).copied();
    !before.is_some_and(|byte| is_ident_byte(byte) || byte == b'.')
        && !after.is_some_and(is_ident_byte)
}

/// Offsets where `code` names one of the wgpu handles in `GPU_HANDLES`, also when the
/// receiver and the field are split by spaces, a line break, or a blanked comment.
fn handle_uses(code: &str) -> Vec<usize> {
    let mut uses = Vec::new();
    for handle in GPU_HANDLES {
        let (receiver, field) = handle.split_once('.').expect("receiver.field handle");
        uses.extend(field_uses(code, receiver, field));
    }
    uses
}

/// The end of the ASCII whitespace that starts at `at`.
fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// Offsets of `receiver.field` in `code` with optional whitespace around the dot. The
/// receiver must not follow an identifier or a dot, and the field must end a word.
fn field_uses(code: &str, receiver: &str, field: &str) -> Vec<usize> {
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    for (at, _) in code.match_indices(receiver) {
        let joined = at
            .checked_sub(1)
            .is_some_and(|before| is_ident_byte(bytes[before]) || bytes[before] == b'.');
        let dot = skip_space(bytes, at + receiver.len());
        if joined || bytes.get(dot) != Some(&b'.') {
            continue;
        }
        let name = skip_space(bytes, dot + 1);
        let end = name + field.len();
        if code.get(name..end) == Some(field) && !bytes.get(end).copied().is_some_and(is_ident_byte)
        {
            found.push(at);
        }
    }
    found
}

/// Offsets of stored-value device reads, such as `self.device.limits()`.
fn device_reads(code: &str) -> Vec<usize> {
    let mut reads = Vec::new();
    for read in DEVICE_READS {
        reads.extend(code.match_indices(*read).map(|(at, _)| at));
    }
    reads
}

/// Offsets where `code` calls the renderer method `callee`.
fn method_calls(code: &str, callee: &str) -> Vec<usize> {
    let mut calls = Vec::new();
    for form in [format!("self.{callee}("), format!("Self::{callee}(")] {
        calls.extend(code.match_indices(form.as_str()).map(|(at, _)| at));
    }
    calls
}

/// Whether `method` reaches the GPU outside its gate, given the methods
/// already known to.
///
/// A gated method may read stored device values and call the
/// [`NOT_GPU_WORK`] readers anywhere; every other handle use, and every call
/// into a reaching method, must sit in a range its gate controls. A method
/// with no controlling gate reaches the GPU if it names a handle or `wgpu::`,
/// or calls a reaching method.
fn reaches_gpu(method: &Method, ranges: &[Range<usize>], reaching: &BTreeSet<String>) -> bool {
    let masked = mask_surface_reads(&method.name, &method.code);
    let code = masked.as_str();
    let calls_into = |skip_readers: bool| {
        let mut calls = Vec::new();
        for callee in reaching {
            if skip_readers && NOT_GPU_WORK.contains(&callee.as_str()) {
                continue;
            }
            calls.extend(method_calls(code, callee));
        }
        calls
    };
    if ranges.is_empty() {
        return !handle_uses(code).is_empty()
            || code.contains("wgpu::")
            || !calls_into(false).is_empty();
    }
    let mut exempt: Vec<Range<usize>> = Vec::new();
    for (name, snippet) in UNGATED_BY_DESIGN {
        if method.name == *name {
            exempt.extend(code.match_indices(*snippet).map(|(at, _)| at..at + snippet.len()));
        }
    }
    let reads = device_reads(code);
    let mut work: Vec<usize> =
        handle_uses(code).into_iter().filter(|at| !reads.contains(at)).collect();
    if method.name == CONSTRUCTOR {
        let (local_work, bootstrap) = constructor_handle_uses(code);
        work.extend(local_work);
        exempt.extend(bootstrap);
    }
    work.extend(calls_into(true));
    work.iter().any(|at| {
        !ranges.iter().any(|range| range.contains(at))
            && !exempt.iter().any(|range| range.contains(at))
    })
}

/// The constructor uses local handles before they become `self` fields. Its
/// pre-device exceptions are exact bootstrap calls, not the whole method or
/// even the whole prefix; configuring a surface anywhere still needs a gate.
fn constructor_handle_uses(code: &str) -> (Vec<usize>, Vec<Range<usize>>) {
    let bootstrap_end = code.find("let format = ").expect("constructor bootstrap boundary");
    let mut exempt = Vec::new();
    for call in ["instance.create_surface(", "instance.request_adapter("] {
        for (at, _) in code[..bootstrap_end].match_indices(call) {
            // Only the instance receiver is exempt; nested argument work still needs its own gate.
            exempt.push(at..at + "instance".len());
        }
    }
    // The shared negotiation receives the instance and surface before any device exists.
    let negotiation = "recovery_context::negotiate_device(&instance, &surface)";
    for (at, _) in code[..bootstrap_end].match_indices(negotiation) {
        exempt.push(at..at + negotiation.len());
    }
    let handlers = "install_device_error_handlers(&device)";
    for (at, _) in code[..bootstrap_end].match_indices(handlers) {
        exempt.push(at..at + handlers.len());
    }
    for (at, _) in code[..bootstrap_end].match_indices("compatible_surface: Some(&surface)") {
        exempt.push(at..at + "compatible_surface: Some(&surface)".len());
    }
    // These access immutable metadata, not GPU work, and are needed to choose initial configuration.
    for read in ["device.limits()", "surface.get_capabilities(&adapter)"] {
        exempt.extend(code.match_indices(read).map(|(at, _)| at..at + read.len()));
    }
    let mut work = Vec::new();
    for handle in ["device", "queue", "surface", "instance"] {
        for (at, _) in code.match_indices(handle) {
            if is_whole_field(code.as_bytes(), at, handle.len())
                && (code.as_bytes().get(at + handle.len()) == Some(&b'.')
                    || at.checked_sub(1).is_some_and(|before| code.as_bytes()[before] == b'&'))
            {
                work.push(at);
            }
        }
    }
    (work, exempt)
}

/// Renderer methods that reach the GPU outside the device gate, directly or
/// through another such method.
fn reaching_methods(methods: &[Method]) -> BTreeSet<String> {
    let gates: Vec<Vec<Range<usize>>> =
        methods.iter().map(|method| gate_ranges(&method.code)).collect();
    let mut reaching: BTreeSet<String> = BTreeSet::new();
    loop {
        let mut added = Vec::new();
        for (method, ranges) in methods.iter().zip(&gates) {
            if !reaching.contains(&method.name) && reaches_gpu(method, ranges, &reaching) {
                added.push(method.name.clone());
            }
        }
        if added.is_empty() {
            return reaching;
        }
        reaching.extend(added);
    }
}

/// Every violation of the device-gate contract in `sources`.
fn gate_violations(sources: &[&str]) -> Vec<String> {
    let blanked: Vec<String> = sources.iter().copied().map(code_only).collect();
    let (blocks, methods) = renderer_methods(&blanked);
    if blocks == 0 {
        return vec!["no `impl GpuRenderer` block".to_owned()];
    }
    let mut violations = Vec::new();
    for name in GATED_METHODS.iter().chain(NOT_GPU_WORK) {
        if !methods.iter().any(|method| method.name == *name) {
            violations.push(format!("no renderer method `{name}`"));
        }
    }
    for name in reaching_methods(&methods) {
        if !NOT_GPU_WORK.contains(&name.as_str()) {
            violations.push(format!("`{name}` reaches the GPU outside the device gate"));
        }
    }
    for method in &methods {
        if GATED_METHODS.contains(&method.name.as_str()) && gate_ranges(&method.code).is_empty() {
            violations.push(format!("`{}` has no gate that controls its GPU work", method.name));
        }
    }
    violations
}

fn block_after(source: &str, header: &str) -> String {
    let source = source.replace("\r\n", "\n");
    let start = source.find(header).unwrap_or_else(|| panic!("missing `{header}`"));
    let block = &source[start..];
    block[..block.find("\n}\n").unwrap_or_else(|| panic!("unterminated `{header}`"))].to_owned()
}

fn without_function(source: &str, signature: &str) -> String {
    let source = source.replace("\r\n", "\n");
    let Some(start) = source.find(signature) else {
        return source;
    };
    let end = source[start..].find("\n}\n").map_or(source.len(), |end| start + end + 3);
    format!("{}{}", &source[..start], &source[end..])
}

/// Every production source file under the crate's `src/`, recursively, with
/// its path relative to `src/`.
fn production_sources() -> Vec<(String, String)> {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![src.clone()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read a gpu source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
            if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
                continue;
            }
            let relative = path.strip_prefix(&src).expect("under src").display().to_string();
            files.push((relative, std::fs::read_to_string(&path).expect("read source file")));
        }
    }
    files.sort();
    files
}

fn banned_calls(name: &str, text: &str) -> Vec<String> {
    let mut offenders = Vec::new();
    for (index, line) in code_only(text).lines().enumerate() {
        for banned in BANNED {
            if line.contains(*banned) {
                offenders.push(format!("{name}:{}: {banned}", index + 1));
            }
        }
    }
    offenders
}

/// Every renderer method that reaches the device runs that work where the
/// gate's result controls it, or only reads the device; Drop issues no GPU
/// work.
///
/// The check is textual. It proves where handle uses and method calls sit
/// relative to a `gpu_work` closure, an `if let` gate body, or the code after a
/// `let`-`else` or propagated gate. It cannot see what a helper outside
/// `impl GpuRenderer` does with an object the renderer already holds, such as a
/// pipeline or an upload, so such a call is checked only when it passes a handle.
#[test]
fn renderer_gpu_work_goes_through_the_gate() {
    let files = production_sources();
    let sources: Vec<&str> = files.iter().map(|(_, text)| text.as_str()).collect();
    let violations = gate_violations(&sources);
    assert!(violations.is_empty(), "device-gate violations: {violations:?}");
    let core = code_only(include_str!("core.rs"));
    let drop_block = block_after(&core, "impl Drop for GpuRenderer {");
    assert!(
        handle_uses(&drop_block).is_empty() && !drop_block.contains("wgpu::"),
        "renderer Drop must issue no GPU work"
    );
}

/// Outside tests and the fault hook's two helpers, every production source
/// under `src/` never polls, never uses render bundles or buffer-init helpers,
/// and pushes no error scope.
#[test]
fn production_code_avoids_fatal_wgpu_paths() {
    let mut offenders = Vec::new();
    for (name, mut text) in production_sources() {
        if name == "device_errors.rs" {
            text = without_function(&text, "pub fn destroy_and_await_loss(");
            text = without_function(&text, "pub fn run_isolated_validation(");
        }
        offenders.extend(banned_calls(&name, &text));
    }
    assert!(offenders.is_empty(), "fatal-path wgpu calls in production code: {offenders:?}");
}

/// Mutation controls: the structural checks pass the renderer as written, and
/// fail on an ungated method in a second `impl GpuRenderer` block, a discarded
/// `accepts_gpu_work()` or gate scope, work outside or before the gate, a
/// `poll_all` call, and a source with no renderer block.
#[test]
fn structural_checks_reject_seeded_defects() {
    let renderer = [
        include_str!("core.rs"),
        include_str!("present.rs"),
        include_str!("rebind.rs"),
        include_str!("recovery_context.rs"),
    ]
    .join("\n");
    assert_eq!(gate_violations(&[renderer.as_str()]), Vec::<String>::new());
    let seeded = |name: &str, body: &str| {
        let method = format!("    fn {name}(&self) {{\n        {body}\n    }}\n");
        format!("{renderer}\nimpl GpuRenderer {{\n{method}}}\n")
    };
    let submit = "self.queue.submit(None);";
    let gate = "self.device_errors.enter_gpu_work(\"seed\")";
    let defects = [
        ("second_block_work", submit.to_owned()),
        ("discarded_check", format!("let _ = self.device_errors.accepts_gpu_work(); {submit}")),
        ("discarded_scope", format!("let _scope = {gate}; {submit}")),
        ("work_outside_closure", format!("self.device_errors.gpu_work(\"x\", || ()); {submit}")),
        ("work_before_gate", format!("{submit} let Some(_s) = {gate} else {{ return; }};")),
        ("seeded_poll", "wgpu::Instance::poll_all(&self.instance, true);".to_owned()),
        ("seeded_poll_method", "self.instance.poll_all(true);".to_owned()),
    ];
    for (name, body) in &defects {
        let source = seeded(name, body.as_str());
        let violations = gate_violations(&[source.as_str()]);
        assert!(
            violations.iter().any(|violation| violation.contains(*name)),
            "{name}: {violations:?}"
        );
        if name.starts_with("seeded_poll") {
            let offenders = banned_calls("seeded.rs", &source);
            assert!(offenders.iter().any(|offender| offender.ends_with("poll_all(")), "{name}");
        }
    }
    let gated_body = format!("let Some(_s) = {gate} else {{ return; }}; {submit}");
    let gated = seeded("seeded_gated", gated_body.as_str());
    assert_eq!(gate_violations(&[gated.as_str()]), Vec::<String>::new());
    let missing = gate_violations(&["fn unrelated() {}"]);
    assert_eq!(missing, vec!["no `impl GpuRenderer` block".to_owned()]);
}

/// CRLF checkout text must have exactly the same extracted blocks, retained
/// suffix and violations as LF, including banned work after an exempt hook.
#[test]
fn structural_checks_are_identical_for_lf_and_crlf() {
    let core = [
        include_str!("core.rs"),
        include_str!("present.rs"),
        include_str!("rebind.rs"),
        include_str!("recovery_context.rs"),
    ]
    .join("\n")
    .replace("\r\n", "\n");
    let windows = core.replace('\n', "\r\n");
    assert_eq!(code_only(&core), code_only(&windows));
    assert_eq!(gate_violations(&[&core]), gate_violations(&[&windows]));
    let header = "impl Drop for GpuRenderer {";
    assert_eq!(block_after(&code_only(&core), header), block_after(&code_only(&windows), header));
    assert_eq!(block_after(&core, header), block_after(&windows, header));
    let source =
        "pub fn exempt() {\n    device.poll();\n}\nfn later() {\n    instance.poll_all(true);\n}\n";
    let lf = without_function(source, "pub fn exempt(");
    let crlf = without_function(&source.replace('\n', "\r\n"), "pub fn exempt(");
    assert_eq!(lf, crlf);
    assert!(lf.contains("fn later()"), "exempting one hook cannot erase later production code");
    assert_eq!(banned_calls("fixture.rs", &lf), banned_calls("fixture.rs", &crlf));
    assert!(banned_calls("fixture.rs", &crlf).iter().any(|error| error.ends_with("poll_all(")));
}

/// A gate in the constructor does not license earlier configure work; a label
/// expression runs before gpu_work can decide whether to invoke its closure.
#[test]
fn constructor_and_gate_arguments_cannot_bypass_containment() {
    let core = [
        include_str!("core.rs"),
        include_str!("present.rs"),
        include_str!("rebind.rs"),
        include_str!("recovery_context.rs"),
    ]
    .join("\n");
    assert!(gate_violations(&[core.as_str()]).is_empty());
    let anchor = "        let init_scope = errors";
    assert_eq!(core.matches(anchor).count(), 1);
    let ungated = core.replacen(
        anchor,
        "        surface.configure(&device, &config);\n        let init_scope = errors",
        1,
    );
    assert!(gate_violations(&[&ungated]).iter().any(|error| error.contains("new_async")));
    let bootstrap_anchor = "        let format = TextureFormat::Bgra8UnormSrgb;";
    let before_bootstrap = core.replacen(
        bootstrap_anchor,
        &format!("        surface.configure(&device, &config);\n{bootstrap_anchor}"),
        1,
    );
    assert!(gate_violations(&[&before_bootstrap]).iter().any(|error| error.contains("new_async")));
    let argument = concat!(
        "\nimpl GpuRenderer {\n    fn argument_work(&self) {\n",
        "        self.device_errors.gpu_work({self.queue.submit(None); \"probe\"},||());\n",
        "    }\n}\n"
    );
    let seeded = format!("{core}{argument}");
    assert!(gate_violations(&[&seeded]).iter().any(|error| error.contains("argument_work")));
    let body = code_only("self.device_errors.gpu_work(\"probe\", || { self.queue.submit(None); })");
    let ranges = gate_ranges(&body);
    assert!(ranges.iter().any(|range| range.contains(&body.find("self.queue").unwrap())));
}

/// Moving native work out of `render_frame` does not exempt any new entry point:
/// ungated work in a delegate or a presenter must still fail structural validation.
#[test]
fn presentation_delegates_and_presenters_remain_in_the_gate_graph() {
    let renderer = [
        include_str!("core.rs"),
        include_str!("present.rs"),
        include_str!("rebind.rs"),
        include_str!("recovery_context.rs"),
    ]
    .join("\n");
    assert!(gate_violations(&[renderer.as_str()]).is_empty());
    for name in [
        "render",
        "render_with_outcome",
        "render_frame",
        "present_frame",
        "prepare_cached_present",
        "present_unchanged_frame",
        "reblit_software_frame",
        "present_software_frame",
        "present_wgpu_frame",
        "finish_surface_retry",
    ] {
        let (_, methods) = renderer_methods(&[code_only(&renderer)]);
        assert!(methods.iter().any(|method| method.name == name), "missing {name}");
        // A second definition is merged by the scanner, exposing work outside the real gate.
        let seeded = format!(
            "{renderer}\nimpl GpuRenderer {{\n    fn {name}(&self) {{ self.queue.submit(None); }}\n}}\n"
        );
        let violations = gate_violations(&[seeded.as_str()]);
        assert!(
            violations.iter().any(|violation| violation.contains(name)),
            "{name} escaped the gate graph: {violations:?}"
        );
    }
}

/// `code` with each method chain split across spaces or lines rejoined around its
/// member dots, so `device` and `.create_buffer(` on two lines read as one call.
/// Comments and strings must already be blanked.
fn join_chains(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    let mut out = String::with_capacity(code.len());
    let mut at = 0;
    while at < chars.len() {
        if !chars[at].is_whitespace() {
            out.push(chars[at]);
            at += 1;
            continue;
        }
        let mut next = at;
        while next < chars.len() && chars[next].is_whitespace() {
            next += 1;
        }
        let before_dot = chars.get(next) == Some(&'.') && chars.get(next + 1) != Some(&'.');
        let after_dot = out.ends_with('.') && !out.ends_with("..");
        if !before_dot && !after_dot {
            out.push(' ');
        }
        at = next;
    }
    out
}

/// Offsets in a normalized negotiation body that use a device handle outside its
/// exact bootstrap calls: the adapter request and the handler installation. A use is a
/// member call on the handle or a borrow of it.
fn negotiation_work(body: &str) -> Vec<usize> {
    let bytes = body.as_bytes();
    let mut exempt = Vec::new();
    for call in ["instance.request_adapter(", "install_device_error_handlers(&device)"] {
        exempt.extend(body.match_indices(call).map(|(at, _)| at..at + call.len()));
    }
    let mut work = Vec::new();
    for handle in ["device", "queue", "surface", "instance"] {
        for (at, _) in body.match_indices(handle) {
            let reaches = is_whole_field(bytes, at, handle.len())
                && (bytes.get(at + handle.len()) == Some(&b'.') || is_borrowed(bytes, at));
            if reaches && !exempt.iter().any(|range| range.contains(&at)) {
                work.push(at);
            }
        }
    }
    work
}

/// Whether the handle at `at` in normalized `code` is borrowed: its previous token is
/// `&`, or a whole `mut` after `&`, with only spaces or blanked comments between them.
fn is_borrowed(code: &[u8], at: usize) -> bool {
    let previous = |end: usize| code[..end].iter().rposition(|byte| !byte.is_ascii_whitespace());
    let mut token = previous(at);
    let after_mut = token.filter(|&last| {
        code[..=last].ends_with(b"mut")
            && !last.checked_sub(3).is_some_and(|before| is_ident_byte(code[before]))
    });
    if let Some(last) = after_mut {
        token = previous(last - 2);
    }
    token.is_some_and(|last| code[last] == b'&')
}

/// The comment-free, chain-joined body of `negotiate_device` in `source`.
fn negotiation_body(source: &str) -> String {
    join_chains(&block_after(&code_only(source), "pub(super) async fn negotiate_device("))
}

/// Startup and recovery request adapters and devices through one function: production
/// code calls `request_adapter` and `request_device` only in `negotiate_device`,
/// which installs the error handlers after the device request and issues no other
/// device work. Comments and split chains are normalized first, so a call on or a borrow
/// of a handle seeded before the handlers fails on one line, across lines, or split by
/// a comment; only the exact install call is exempt, and a name containing a handle
/// passes.
#[test]
fn device_negotiation_has_one_bootstrap_path() {
    let squeeze = |text: &str| text.split_whitespace().collect::<String>();
    let mut requests = Vec::new();
    for (name, text) in production_sources() {
        let code = join_chains(&code_only(&text));
        for call in [".request_adapter(", ".request_device("] {
            requests.extend(code.matches(call).map(|_| format!("{name}: {call}")));
        }
    }
    assert_eq!(
        requests,
        ["recovery_context.rs: .request_adapter(", "recovery_context.rs: .request_device("]
    );
    let core = squeeze(&code_only(include_str!("core.rs")));
    assert_eq!(core.matches("recovery_context::negotiate_device(&instance,&surface)").count(), 1);
    let context = include_str!("recovery_context.rs");
    let run = "negotiate_device(&surface.instance,&surface.surface)";
    assert_eq!(squeeze(&code_only(context)).matches(run).count(), 1);
    let body = negotiation_body(context);
    assert_eq!(negotiation_work(&body), Vec::<usize>::new());
    let request = body.find(".request_device(").expect("device request");
    let install = body.find("install_device_error_handlers(&device)").expect("handler install");
    assert!(request < install, "the handlers must follow the device request");
    let anchor = "    let device_errors = install_device_error_handlers(&device);";
    assert_eq!(context.matches(anchor).count(), 1);
    for seed in [
        "device.create_buffer(&probe);",
        "device\n        .create_buffer(&probe);",
        "device /* seeded */ .create_buffer(&probe);",
        "queue\n        // seeded\n        .submit(None);",
        "create_frame_texture(&device, 1, 1, TextureFormat::Bgra8UnormSrgb);",
        "create_frame_texture(&\n        device, 1, 1, TextureFormat::Bgra8UnormSrgb);",
        "create_frame_texture(& /* gap */ device, 1, 1, TextureFormat::Bgra8UnormSrgb);",
        "probe(&mut /* gap */\n        queue);",
        "let early = install_device_error_handlers(& /* gap */ device);",
    ] {
        let seeded = context.replacen(anchor, &format!("    {seed}\n{anchor}"), 1);
        assert!(!negotiation_work(&negotiation_body(&seeded)).is_empty(), "{seed:?} must fail");
    }
    let partial = context.replacen(
        anchor,
        &format!("    probe(& /* gap */ device_count, &mut surfaces);\n{anchor}"),
        1,
    );
    assert_eq!(negotiation_work(&negotiation_body(&partial)), Vec::<usize>::new());
}

/// The renderer sources with the recovery rebind and context files joined in.
fn recovery_renderer() -> String {
    [
        include_str!("core.rs"),
        include_str!("present.rs"),
        include_str!("rebind.rs"),
        include_str!("recovery_context.rs"),
    ]
    .join("\n")
}

/// The device-gate graph covers a candidate context's device, queue, and instance and
/// a candidate surface, not only the renderer's own handles: device work seeded
/// before the prepare or commit gate fails, also when the receiver and field are split
/// by a line break, spaces, or a comment.
#[test]
fn candidate_handles_before_a_rebind_gate_fail_the_graph() {
    let renderer = recovery_renderer();
    assert_eq!(gate_violations(&[renderer.as_str()]), Vec::<String>::new());
    let anchors = [
        ("prepare_rebind", "        let errors = &context.device_errors;\n"),
        ("commit_rebind", "        let errors = Arc::clone(&context.device_errors);\n"),
    ];
    let seeds = [
        "let early = create_frame_texture(&context.device /* seeded */, 1, 1, TextureFormat::Bgra8UnormSrgb);",
        "let early = context\n            .device\n            .create_command_encoder(&Default::default());",
        "let early = context . queue . submit(None);",
        "let early = surface . surface . get_capabilities(&context.adapter);",
        "let early = context.instance.create_surface(self.window.clone());",
    ];
    for (method, anchor) in anchors {
        assert_eq!(renderer.matches(anchor).count(), 1, "{method} anchor");
        for seed in seeds {
            let seeded = renderer.replacen(anchor, &format!("        {seed}\n{anchor}"), 1);
            let violations = gate_violations(&[seeded.as_str()]);
            assert!(
                violations.iter().any(|violation| violation.contains(method)),
                "{method} {seed:?}: {violations:?}"
            );
        }
    }
}

/// The recovery surface helper's exact read-only operations mask nothing else: device
/// or queue work, another operation on the old surface, a `wgpu::` path beyond the
/// two allowed spellings, or a call into a GPU-reaching helper still fails the graph,
/// split or not.
#[test]
fn surface_read_exceptions_mask_no_other_gpu_work() {
    let renderer = recovery_renderer();
    let anchor = "        let window = Arc::clone(&self.window);\n";
    assert_eq!(renderer.matches(anchor).count(), 1);
    let helper = "\nimpl GpuRenderer {\n    fn seeded_reaching(&self) {\n        self.queue.submit(None);\n    }\n}\n";
    for seed in [
        "self.device.create_buffer(&probe);",
        "self\n            .queue\n            .submit(None);",
        "self.surface.configure(&self.device, &self.config);",
        "self.surface /* seeded */ .get_capabilities(&self.adapter);",
        "let probe = wgpu::util::TextureBlitter::new;",
        "self.seeded_reaching();",
    ] {
        let seeded = renderer.replacen(anchor, &format!("        {seed}\n{anchor}"), 1);
        let violations = gate_violations(&[format!("{seeded}{helper}").as_str()]);
        assert!(
            violations.iter().any(|violation| violation.contains("candidate_surface")),
            "{seed:?}: {violations:?}"
        );
    }
}

/// Destroying a retired context checks the token's generation first and closes the
/// gate before the device is destroyed; nothing is polled.
#[test]
fn retired_context_destroy_closes_the_gate_first() {
    let context = code_only(include_str!("recovery_context.rs"));
    let body = block_after(&context, "pub fn destroy_retired(");
    let at = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("missing `{needle}`"));
    let check = at("retired.generation() != self.device_errors.generation()");
    let close = at("self.device_errors.request_destroy()");
    let destroy = at("self.device.destroy()");
    assert!(check < close && close < destroy);
    assert!(!body.contains(".poll("));
}
