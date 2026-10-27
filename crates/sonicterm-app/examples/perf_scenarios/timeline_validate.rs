//! Echo timeline causal validation and event binding.

use super::*;

impl Held {
    /// Whether a permit is held.
    pub(super) fn held(self) -> bool {
        self != Self::Absent
    }
}

impl Held {
    /// Whether a clear or a consumption naming `tick_seq` agrees with this state: a known permit is
    /// named by its own tick, an unknown one by none, and an absent one by nothing.
    pub(super) fn named_by(self, tick_seq: Option<u32>) -> bool {
        match (self, tick_seq) {
            (Self::Known(identity, _), Some(tick_seq)) => identity.tick_seq == tick_seq,
            (Self::Unknown, None) => true,
            _ => false,
        }
    }
}

/// The event-loop sequence of an event that carries one.
pub(super) fn loop_of(kind: &EventKind) -> Option<u32> {
    match *kind {
        EventKind::Check { loop_seq, .. }
        | EventKind::Tick { loop_seq, .. }
        | EventKind::Admission { loop_seq, .. }
        | EventKind::Cleared { loop_seq, .. }
        | EventKind::Entry { loop_seq, .. }
        | EventKind::Return { loop_seq, .. } => Some(loop_seq),
        EventKind::ChunkRead | EventKind::RenderEnter { .. } | EventKind::RenderExit { .. } => None,
    }
}

/// Whether two consecutive event-loop events may share one `loop_seq`: only one hook's own events, a
/// streaming check before its admission decision or discard, and a decision before its discard.
pub(super) fn may_share_loop(previous: &EventKind, next: &EventKind) -> bool {
    matches!(
        (previous, next),
        (EventKind::Check { .. }, EventKind::Admission { .. } | EventKind::Cleared { .. })
            | (EventKind::Admission { .. }, EventKind::Cleared { .. })
    )
}

/// Validate the raw record before any population is derived, naming its first violation:
/// - `loop-sequence`: event-loop sequences never decrease in record order, and only one hook's events
///   share one;
/// - `dispatch-pair`: each dispatch has one entry and one return, overlaps no other, and none is left open;
/// - `admission-orphan`, `admission-count`, `admission-order`: one admission per dispatch, inside it,
///   at or after its entry and at or before its render and return;
/// - `render-pair`: one enter and one exit inside an open, admitted dispatch, in time order;
/// - `loop-time-order`: event-loop events and render boundaries never go back in time; equal times are
///   allowed, and `LatestChunkRead`, a historical worker time, is exempt;
/// - `initial-permit`, `tick-identity`, `tick-sequence`: a known permit held at arm was delivered at or
///   before the arm; a known tick identity is nonzero; known accepted ticks strictly increase their
///   `tick_seq` and never decrease their generation, from the permit held at arm on, and none follows
///   an unknown accepted tick, since both numbers only grow and an unknown one is past `u32::MAX`;
/// - `permit-transition`: a clear or consumption names the permit actually held;
/// - `flood-sequence`: services strictly increase, share no sequence with a tick, admission, clear or
///   boundary, and lie past their eviction mark.
///
/// Timestamps are not required to increase globally: `LatestChunkRead` carries a historical worker time.
pub(super) fn validate(record: &Record, instants: &[Instant]) -> Result<Validated, &'static str> {
    let mut dispatches: BTreeMap<u32, Dispatch> = BTreeMap::new();
    let mut open: Option<u32> = None;
    let mut previous_loop: Option<(u32, &EventKind)> = None;
    let mut boundary_loops = std::collections::BTreeSet::new();
    let mut held = match (record.initial_permit, record.initial_permit_unknown) {
        (Some(permit), _) => Held::Known(permit.identity, permit.delivered_at),
        (None, true) => Held::Unknown,
        (None, false) => Held::Absent,
    };
    if let Some(permit) = record.initial_permit {
        if permit.delivered_at > record.armed_at {
            // When: the permit held at arm was delivered after the arm, it cannot have been held then.
            return Err("initial-permit");
        }
        if permit.identity.tick_seq == 0 || permit.identity.generation == 0 {
            // When: the identity is zero, no accepted tick produced it; both numbers start at one.
            return Err("tick-identity");
        }
    }
    let mut transitions = vec![(record.armed_at, held)];
    let mut consumed = BTreeMap::new();
    // The last event-loop instant, the last known accepted tick, and whether an unknown one was accepted.
    let mut last_time: Option<Instant> = None;
    let mut last_known = record.initial_permit.map(|permit| permit.identity);
    let mut unknown_seen = false;
    for (event, at) in record.events.iter().zip(instants) {
        let at = *at;
        if let Some(loop_seq) = loop_of(&event.kind) {
            let shared = previous_loop.is_some_and(|(last, last_kind)| {
                loop_seq < last || (loop_seq == last && !may_share_loop(last_kind, &event.kind))
            });
            if shared {
                // When: a sequence goes back, or two hooks share one, the loop order is malformed.
                return Err("loop-sequence");
            }
            previous_loop = Some((loop_seq, &event.kind));
            if !matches!(event.kind, EventKind::Check { .. }) {
                boundary_loops.insert(loop_seq);
            }
        }
        // The open dispatch an event names, if it names the open one.
        let current = |dispatch_seq: u32| (open == Some(dispatch_seq)).then_some(dispatch_seq);
        match event.kind {
            EventKind::Entry { loop_seq, dispatch_seq } => {
                if open.is_some() || dispatches.contains_key(&dispatch_seq) {
                    // When: a dispatch is still open or its sequence was used, the boundaries are malformed.
                    return Err("dispatch-pair");
                }
                let dispatch = Dispatch {
                    entry_loop: loop_seq,
                    entry: at,
                    returned: None,
                    enter: None,
                    exit: None,
                    decision: None,
                    admitted_at: None,
                };
                dispatches.insert(dispatch_seq, dispatch);
                open = Some(dispatch_seq);
            }
            EventKind::Return { loop_seq, dispatch_seq } => {
                let seq = current(dispatch_seq).ok_or("dispatch-pair")?;
                let dispatch = dispatches.get_mut(&seq).ok_or("dispatch-pair")?;
                if dispatch.enter.is_some() != dispatch.exit.is_some() {
                    // When: the render pair is unpaired at the return, the render boundary is malformed.
                    return Err("render-pair");
                }
                if dispatch.admitted_at.is_some_and(|admitted| at < admitted) {
                    // When: the return precedes its own admission decision, the decision is out of order.
                    return Err("admission-order");
                }
                if at < dispatch.exit.unwrap_or(dispatch.entry) {
                    // When: the return precedes the render or the entry, the dispatch is out of order.
                    return Err("dispatch-pair");
                }
                dispatch.returned = Some((loop_seq, at));
                open = None;
            }
            EventKind::Admission { dispatch_seq, decision, .. } => {
                let seq = current(dispatch_seq).ok_or("admission-orphan")?;
                let dispatch = dispatches.get_mut(&seq).ok_or("admission-orphan")?;
                if dispatch.decision.is_some() {
                    // When: the dispatch already has a decision, a second one is malformed, never a choice.
                    return Err("admission-count");
                }
                if dispatch.enter.is_some() || at < dispatch.entry {
                    // When: the decision follows the render or precedes the entry, it is out of order.
                    return Err("admission-order");
                }
                dispatch.decision = Some(decision);
                dispatch.admitted_at = Some(at);
                if let Decision::Permit { tick_seq } = decision {
                    if !held.named_by(tick_seq) {
                        // When: the consumption names a permit other than the one held, it is impossible.
                        return Err("permit-transition");
                    }
                    consumed.insert(seq, held);
                    held = Held::Absent;
                    transitions.push((at, held));
                }
            }
            EventKind::Cleared { tick_seq, .. } => {
                if !held.named_by(tick_seq) {
                    // When: the clear names a permit other than the one held, or none is held, it is impossible.
                    return Err("permit-transition");
                }
                held = Held::Absent;
                transitions.push((at, held));
            }
            EventKind::Tick { identity, .. } => {
                if let Some(identity) = identity {
                    if identity.tick_seq == 0 || identity.generation == 0 {
                        // When: the identity is zero, no accepted tick produced it; both numbers start at one.
                        return Err("tick-identity");
                    }
                    let follows = last_known.is_none_or(|last| {
                        identity.tick_seq > last.tick_seq && identity.generation >= last.generation
                    });
                    if unknown_seen || !follows {
                        // When: a known tick repeats, goes back, or follows an unknown one, the sequence is impossible.
                        return Err("tick-sequence");
                    }
                    last_known = Some(identity);
                } else {
                    // When: the identity is None, every later accepted tick is past `u32::MAX` too.
                    unknown_seen = true;
                }
                held = identity.map_or(Held::Unknown, |identity| Held::Known(identity, at));
                transitions.push((at, held));
            }
            EventKind::RenderEnter { dispatch_seq } => {
                let seq = current(dispatch_seq).ok_or("render-pair")?;
                let dispatch = dispatches.get_mut(&seq).ok_or("render-pair")?;
                let admitted = matches!(
                    dispatch.decision,
                    Some(Decision::Permit { .. } | Decision::Fallback | Decision::Admitted)
                );
                if dispatch.enter.is_some() || !admitted || at < dispatch.entry {
                    // When: a second enter, an unadmitted render or an enter before the entry, it is malformed.
                    return Err("render-pair");
                }
                if dispatch.admitted_at.is_some_and(|admitted| at < admitted) {
                    // When: the render precedes its own admission decision, the decision is out of order.
                    return Err("admission-order");
                }
                dispatch.enter = Some(at);
            }
            EventKind::RenderExit { dispatch_seq } => {
                let seq = current(dispatch_seq).ok_or("render-pair")?;
                let dispatch = dispatches.get_mut(&seq).ok_or("render-pair")?;
                if dispatch.exit.is_some() || dispatch.enter.is_none_or(|enter| at < enter) {
                    // When: a second exit, an exit with no enter or one before it, the render is malformed.
                    return Err("render-pair");
                }
                dispatch.exit = Some(at);
            }
            EventKind::Check { .. } | EventKind::ChunkRead => {}
        }
        // The chunk read carries a historical worker time; every other event is in event-loop order.
        let historical = event.kind == EventKind::ChunkRead;
        if !historical && last_time.is_some_and(|last| at < last) {
            // When: an event-loop event or render boundary precedes the previous one, time went back.
            return Err("loop-time-order");
        }
        if !historical {
            last_time = Some(at);
        }
    }
    if open.is_some() {
        // When: a dispatch is still open at the take, its return is missing.
        return Err("dispatch-pair");
    }
    let mut last_service = None;
    for (loop_seq, _) in &record.flood {
        if last_service.is_some_and(|last| *loop_seq <= last) || boundary_loops.contains(loop_seq) {
            // When: services repeat, go back, or share a non-check sequence, the flood ring is malformed.
            return Err("flood-sequence");
        }
        last_service = Some(*loop_seq);
    }
    let evicted_past = record
        .flood_evicted_through
        .zip(record.flood.first())
        .is_some_and(|(evicted, (first, _))| evicted >= *first);
    if evicted_past {
        // When: a retained service is at or before the eviction mark, the ring's loss record is malformed.
        return Err("flood-sequence");
    }
    Ok(Validated { dispatches, transitions, consumed })
}

/// Bind the credited forward `started..=ended` to exactly one complete watched dispatch pair, which
/// must hold a render pair.
pub(super) fn bind(
    dispatches: &BTreeMap<u32, Dispatch>,
    started: Instant,
    ended: Instant,
) -> Binding {
    let inside = |at: Instant| started <= at && at <= ended;
    let candidates: Vec<(u32, &Dispatch)> = dispatches
        .iter()
        .filter(|(_, dispatch)| {
            inside(dispatch.entry) && dispatch.returned.is_some_and(|(_, at)| inside(at))
        })
        .map(|(dispatch_seq, dispatch)| (*dispatch_seq, dispatch))
        .collect();
    let [(dispatch_seq, dispatch)] = candidates.as_slice() else {
        return if candidates.is_empty() { Binding::NoPair } else { Binding::Ambiguous };
    };
    let (Some((_, returned)), Some(enter), Some(exit), Some(decision)) =
        (dispatch.returned, dispatch.enter, dispatch.exit, dispatch.decision)
    else {
        // When: the one candidate rendered nothing, none of its frames can be credited.
        return Binding::NoPair;
    };
    Binding::Bound(Bound {
        dispatch_seq: *dispatch_seq,
        entry_loop: dispatch.entry_loop,
        entry: dispatch.entry,
        enter,
        exit,
        returned,
        decision,
    })
}

/// The site of a check at `check_loop` among validated, complete intervals: inside exactly one watched
/// redraw interval is the admission check, outside every one the output service.
pub(super) fn site_of(dispatches: &BTreeMap<u32, Dispatch>, check_loop: u32) -> Option<Site> {
    let mut containing = 0_usize;
    for dispatch in dispatches.values() {
        let (return_loop, _) = dispatch.returned?;
        containing += usize::from(dispatch.entry_loop < check_loop && check_loop < return_loop);
    }
    match containing {
        0 => Some(Site::OutputService),
        1 => Some(Site::Admission),
        _ => None,
    }
}

/// The ordering of a bound sample with a valid split and a readiness check.
pub(super) fn chain(
    credited: &Credited,
    split: &Split,
    bound: &Bound,
    frame: FrameRequest,
) -> Disposition {
    let parsed_at = credited.injected.checked_add(Duration::from_nanos(split.input_to_parse_ns));
    let published_at = parsed_at
        .and_then(|parsed| parsed.checked_add(Duration::from_nanos(split.parse_to_publication_ns)));
    let Some(published_at) = published_at else {
        // When: `published_at` is not representable, no order can be checked.
        return (OrderingStatus::ClockOrder, Some("unrepresentable-instant"), None);
    };
    if bound.entry < published_at && published_at <= bound.returned {
        // When: the publication fell inside the credited dispatch, the sample is context only.
        return (OrderingStatus::PublishedDuringDispatch, None, None);
    }
    if frame.loop_seq > bound.entry_loop {
        // When: readiness came after the credited entry by loop order, it is valid context, not a reversal.
        return (OrderingStatus::SplitOnly, Some("readiness-after-entry"), None);
    }
    if frame.at < published_at {
        // When: readiness preceded the publication, it is X1's named case, never forced into the order.
        return (OrderingStatus::SplitOnly, Some("ready-before-publication"), None);
    }
    let parts = (
        ns_between(published_at, frame.at),
        ns_between(frame.at, bound.entry),
        ns_between(bound.entry, bound.enter),
        ns_between(bound.enter, bound.exit),
        ns_between(bound.exit, credited.ended),
        ns_between(bound.exit, bound.returned),
        ns_between(bound.returned, credited.ended),
        ns_between(credited.injected, credited.ended),
    );
    let (
        Some(publication_to_frame_request),
        Some(frame_request_to_entry),
        Some(entry_to_render),
        Some(render),
        Some(render_exit_to_credited_end),
        Some(exit_to_return),
        Some(return_to_end),
        Some(credited_ns),
    ) = parts
    else {
        return (OrderingStatus::ClockOrder, Some("event-order"), None);
    };
    let additive_ns = [
        split.input_to_parse_ns,
        split.parse_to_publication_ns,
        publication_to_frame_request,
        frame_request_to_entry,
        entry_to_render,
        render,
        render_exit_to_credited_end,
    ];
    let total = additive_ns.iter().try_fold(0_u64, |sum, part| sum.checked_add(*part));
    let tail = exit_to_return.checked_add(return_to_end);
    if total != Some(credited_ns) || tail != Some(render_exit_to_credited_end) {
        // When: the parts do not sum exactly in integer nanoseconds, the sample is malformed.
        return (OrderingStatus::ClockOrder, Some("sum"), None);
    }
    let parts = Parts {
        additive_ns,
        render_exit_to_dispatch_return_ns: exit_to_return,
        dispatch_return_to_credited_end_ns: return_to_end,
    };
    (OrderingStatus::Ordered, None, Some(parts))
}

/// The permit's present and absent time over `from..until`, integrated over the validated transitions.
pub(super) fn occupancy(
    transitions: &[(Instant, Held)],
    from: Instant,
    until: Instant,
) -> Option<(u64, u64)> {
    let (mut present, mut absent) = (0_u64, 0_u64);
    for (index, (start, held)) in transitions.iter().enumerate() {
        let end = transitions.get(index + 1).map_or(until, |(next, _)| *next);
        // A segment outside the window reverses once clipped and counts zero.
        let span = ns_between((*start).max(from), end.min(until)).unwrap_or(0);
        let total = if held.held() { &mut present } else { &mut absent };
        *total = total.checked_add(span)?;
    }
    Some((present, absent))
}
