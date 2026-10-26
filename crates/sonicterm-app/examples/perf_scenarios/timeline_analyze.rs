//! Echo timeline credited samples, decomposition and coverage verdicts.

use super::*;

/// Reduce one credited sample's timeline to its schema-1 fields.
pub(crate) fn analyze(take: &TimelineTake, credited: &Credited) -> TimelineSample {
    let record = match take {
        TimelineTake::Unavailable(reason) => {
            return TimelineSample::bare(Availability::Unavailable, Some(reason), credited);
        }
        TimelineTake::Rejected(reason) => {
            return TimelineSample::bare(Availability::Rejected, Some(reason), credited);
        }
        TimelineTake::Recorded(record) => record,
    };
    if record.initial_permit.is_some() && record.initial_permit_unknown {
        // When: both initial-permit forms are set, the record is malformed and rejected.
        return TimelineSample::bare(
            Availability::Rejected,
            Some("malformed-initial-permit"),
            credited,
        );
    }
    let mut sample = TimelineSample::bare(Availability::Recorded, None, credited);
    sample.overflow = Some(record.overflow);
    let chunk_read = record.events.iter().find(|event| event.kind == EventKind::ChunkRead);
    sample.read_stamp = Some(match (record.chunk_timestamps, chunk_read) {
        (false, _) => ReadStamp::NotConfigured,
        (true, Some(_)) => ReadStamp::Stamped,
        (true, None) => ReadStamp::MissingOrUnrepresentable,
    });
    let split = credited.split.filter(|_| credited.split_reason == crate::record::SPLIT);
    let parsed_at = split.and_then(|split| {
        credited.injected.checked_add(Duration::from_nanos(split.input_to_parse_ns))
    });
    let read_at =
        chunk_read.and_then(|event| record.armed_at.checked_add(Duration::from_nanos(event.at_ns)));
    // A lower bound; a missing or later read is null, never zero.
    sample.latest_read_to_parse_ns =
        read_at.zip(parsed_at).and_then(|(read, parsed)| ns_between(read, parsed));
    let existing = match credited.split_reason {
        crate::record::SPLIT => None,
        "clock-order" => Some((OrderingStatus::ClockOrder, "clock-order")),
        reason => Some((OrderingStatus::SplitOnly, reason)),
    };
    let decide = |sample: &mut TimelineSample, (status, reason): (OrderingStatus, &'static str)| {
        sample.ordering = Some(status);
        sample.ordering_reason = Some(reason);
    };
    if record.overflow {
        // When: an event was dropped, every event-dependent field stays null.
        decide(&mut sample, existing.unwrap_or((OrderingStatus::SplitOnly, "overflow")));
        return sample;
    }
    let instants: Option<Vec<Instant>> = record
        .events
        .iter()
        .map(|event| record.armed_at.checked_add(Duration::from_nanos(event.at_ns)))
        .collect();
    let Some(instants) = instants else {
        // When: an event's instant is not representable, the sample's order cannot be read.
        decide(&mut sample, (OrderingStatus::ClockOrder, "unrepresentable-instant"));
        return sample;
    };
    // The raw record is validated before any population is derived, so no named ordering case can
    // mask a malformed sequence.
    let validated = match validate(record, &instants) {
        Ok(validated) => validated,
        Err(reason) => {
            // When: the recorded execution is malformed, it stays recorded as `clock-order` with its reason,
            // derives nothing, and leaves every event-dependent population.
            decide(&mut sample, (OrderingStatus::ClockOrder, reason));
            return sample;
        }
    };
    let binding = bind(&validated.dispatches, credited.forward_started, credited.ended);
    let bound = match binding {
        Binding::Bound(bound) => Some(bound),
        Binding::NoPair | Binding::Ambiguous => None,
    };
    let decision = bound.map(|bound| bound.decision);
    sample.credited_dispatch_seq = bound.map(|bound| bound.dispatch_seq);
    sample.admission = decision.and_then(|decision| match decision {
        Decision::Permit { .. } => Some(Admission::Permit),
        Decision::Fallback => Some(Admission::Fallback),
        Decision::Admitted => Some(Admission::Admitted),
        Decision::Deferred | Decision::Held => None,
    });
    if let Some(bound) = bound {
        // The consumed permit is the one validation found held at the credited admission.
        let consumed = validated.consumed.get(&bound.dispatch_seq).copied();
        sample.permit_identity = Some(match consumed {
            None => PermitIdentity::None,
            Some(Held::Known(..)) => PermitIdentity::Known,
            // Validation admits only a held permit's consumption, so this is the unknown one.
            Some(Held::Unknown | Held::Absent) => PermitIdentity::Unknown,
        });
        let delivery = match consumed {
            Some(Held::Known(_, delivered)) => Some(delivered),
            _ => None,
        };
        sample.tick_to_entry_ns = delivery.and_then(|delivered| ns_between(delivered, bound.entry));
        sample.tick_qualified = Some(sample.tick_to_entry_ns.is_some());
    }
    let frame = split.and_then(|split| {
        record.events.iter().zip(&instants).find_map(|(event, at)| match event.kind {
            EventKind::Check { loop_seq, generation, outcome }
                if generation >= split.echo_generation =>
            {
                Some(FrameRequest { at: *at, loop_seq, outcome })
            }
            _ => None,
        })
    });
    sample.readiness = frame.map(|frame| Readiness {
        outcome: frame.outcome,
        loop_seq: frame.loop_seq,
        site: site_of(&validated.dispatches, frame.loop_seq),
    });
    let published_at = split.zip(parsed_at).and_then(|(split, parsed)| {
        parsed.checked_add(Duration::from_nanos(split.parse_to_publication_ns))
    });
    sample.ready_before_publication =
        frame.zip(published_at).map(|(frame, published)| frame.at < published);
    let (status, reason, parts) = match (existing, binding, split, frame) {
        (Some((status, reason)), ..) => (status, Some(reason), None),
        (None, Binding::NoPair, ..) => (OrderingStatus::SplitOnly, Some("no-credited-pair"), None),
        (None, Binding::Ambiguous, ..) => {
            (OrderingStatus::SplitOnly, Some("ambiguous-credited-pair"), None)
        }
        (None, Binding::Bound(_), _, None) | (None, Binding::Bound(_), None, _) => {
            (OrderingStatus::SplitOnly, Some("missing-event:output-check"), None)
        }
        (None, Binding::Bound(bound), Some(split), Some(frame)) => {
            chain(credited, &split, &bound, frame)
        }
    };
    sample.ordering = Some(status);
    sample.ordering_reason = reason;
    sample.parts = parts;
    // Permit occupancy and the flood count cover readiness to entry; readiness after entry supplies neither.
    if let (Some(bound), Some(frame), Some(decision)) = (bound, frame, decision) {
        if frame.loop_seq <= bound.entry_loop && frame.at <= bound.entry {
            if decision != Decision::Fallback {
                let occupied = occupancy(&validated.transitions, frame.at, bound.entry);
                sample.permit_present_ns = occupied.map(|(present, _)| present);
                sample.permit_absent_ns = occupied.map(|(_, absent)| absent);
            }
            let first = frame.loop_seq.checked_add(1);
            let in_interval = |loop_seq: u32| {
                first.is_some_and(|first| first <= loop_seq && loop_seq <= bound.entry_loop)
            };
            sample.m3_complete = Some(first.is_none_or(|first| {
                record.flood_evicted_through.is_none_or(|evicted| evicted < first)
            }));
            let served = record
                .flood
                .iter()
                .filter(|(loop_seq, pane)| *pane != credited.pane && in_interval(*loop_seq))
                .count();
            sample.flood_services = u64::try_from(served).ok();
        }
    }
    sample
}

/// The timeline of a credited sample whose watch was not armed: none out of scope, else unavailable
/// with the arm's reason. An armed sample's timeline comes from its take.
pub(crate) fn unarmed_timeline(arm: ArmState) -> Option<TimelineTake> {
    match arm {
        ArmState::OutOfScope | ArmState::Armed(_) => None,
        ArmState::Unsupported => Some(TimelineTake::Unavailable("cfg-off")),
        ArmState::Failed(reason) => {
            Some(TimelineTake::Unavailable(reason.strip_prefix("arm-").unwrap_or(reason)))
        }
    }
}

/// The `echo_timeline_coverage` object over credited samples' timelines, each population with its named
/// numerator and denominator: B2 and M3 over ordered and, again, over credited. Null when no credited
/// sample was in scope; `unavailable`, with null populations, when none was recorded, never a measured zero.
pub(crate) fn coverage<'sample>(
    timelines: impl IntoIterator<Item = &'sample TimelineSample>,
) -> Value {
    let samples: Vec<&TimelineSample> = timelines.into_iter().collect();
    let credited = samples.len();
    if credited == 0 {
        // When: no credited sample was in the timeline's scope, there is no coverage to report.
        return Value::Null;
    }
    let count = |member: fn(&TimelineSample) -> bool| {
        samples.iter().filter(|sample| member(sample)).count()
    };
    let recorded = count(|sample| sample.availability == Availability::Recorded);
    let population = |numerator: usize, denominator: usize| json!({"numerator": numerator, "denominator": denominator});
    if recorded == 0 {
        // When: nothing was recorded, the side is unavailable rather than a population of zero.
        return json!({
            "schema": TIMELINE_SCHEMA,
            "availability": "unavailable",
            "credited": credited,
            "recorded": population(0, credited),
            "ordered": Value::Null,
            "b2": Value::Null,
            "m4": Value::Null,
            "m3": Value::Null,
            "read_stamped": Value::Null,
            "b2_of_credited": Value::Null,
            "m3_of_credited": Value::Null,
        });
    }
    let ordered = count(TimelineSample::ordered);
    json!({
        "schema": TIMELINE_SCHEMA,
        "availability": "available",
        "credited": credited,
        "recorded": population(recorded, credited),
        "ordered": population(ordered, credited),
        "b2": population(count(TimelineSample::in_b2), ordered),
        "m4": population(count(TimelineSample::in_m4), ordered),
        "m3": population(count(TimelineSample::in_m3), ordered),
        "read_stamped": population(count(TimelineSample::read_stamped), ordered),
        "b2_of_credited": population(count(TimelineSample::in_b2), credited),
        "m3_of_credited": population(count(TimelineSample::in_m3), credited),
    })
}
