//! Pins OSC 7501 program status: kitty's grammar and limits (discard versus skip), the fixed query
//! reply, the 32-record store with its two clocks, the prompt, exit, acknowledgment and RIS
//! lifetimes, and the one summary event per batch.

use base64::Engine;
use sonicterm_grid::grid::Grid;

use super::*;
use crate::vt::{CaptureStagingPool, Parser, VtEvent};

/// A parser with a private staging pool, as the crate's unit tests require.
fn parser() -> Parser {
    Parser::new_with_staging_pool(Grid::new(40, 4), None, CaptureStagingPool::new())
}

/// `text` in standard padded base64, as programs encode `title` and `msg`.
fn encode(text: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

/// Feed one `7501;` report with `body`, BEL-terminated.
fn report(parser: &mut Parser, body: &str) -> Vec<VtEvent> {
    parser.advance(format!("\x1b]7501;{body}\x07").as_bytes())
}

/// Every record as `(id, state, kind, progress, app, title, msg)`, oldest change first.
fn records(parser: &Parser) -> Vec<(String, &'static str, Option<&'static str>, Option<u8>)> {
    parser
        .program_status()
        .records()
        .map(|record| {
            (
                record.id().to_owned(),
                record.state().as_str(),
                record.kind().map(StatusKind::as_str),
                record.progress(),
            )
        })
        .collect()
}

/// The query is answered with kitty's fixed reply, ended by the request's own terminator, once per
/// query, at most 80 bytes, and it stores nothing.
#[test]
fn the_query_reply_is_fixed_and_uses_the_request_terminator() {
    for (terminator, reply_end) in
        [(b"\x07".as_slice(), b"\x07".as_slice()), (b"\x1b\\", b"\x1b\\")]
    {
        let mut parser = parser();
        let mut query = b"\x1b]7501;?".to_vec();
        query.extend_from_slice(terminator);
        let (_, _, replies) = parser.advance_with_replies(&query);
        let mut expected =
            b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth"
                .to_vec();
        expected.extend_from_slice(reply_end);
        assert_eq!(replies, expected, "{terminator:?}");
        assert!(replies.len() <= 80, "the reply is at most 80 bytes");
        assert!(parser.program_status().is_empty(), "a query stores nothing");
    }
}

/// Each of the five states is stored under the root record; `kind` is kept only for `blocked`
/// and `progress` (0-100) only for `working` and `blocked`.
#[test]
fn each_state_keeps_only_its_own_fields() {
    let mut parser = parser();
    for state in ["idle", "working", "done", "blocked", "error"] {
        report(&mut parser, &format!("state={state}:kind=question:progress=40"));
        let expected_kind = (state == "blocked").then_some("question");
        let expected_progress = matches!(state, "working" | "blocked").then_some(40);
        assert_eq!(
            records(&parser),
            vec![(String::new(), state, expected_kind, expected_progress)],
            "{state}"
        );
    }
    report(&mut parser, "state=working:progress=101");
    assert_eq!(records(&parser)[0].3, None, "progress past 100 is dropped");
    report(&mut parser, "state=blocked:kind=other:progress=-1");
    assert_eq!(records(&parser)[0], (String::new(), "blocked", None, None));
}

/// An unknown or missing state discards the whole report, so the earlier record stays.
#[test]
fn an_unknown_state_discards_the_report() {
    let mut parser = parser();
    report(&mut parser, "state=working");
    for body in ["state=paused", "state=", "app=x", "STATE=idle"] {
        report(&mut parser, body);
        assert_eq!(records(&parser), vec![(String::new(), "working", None, None)], "{body}");
    }
}

/// The Claude Code forms: a blocked permission report, then a clear of a named record.
#[test]
fn the_claude_code_forms_apply() {
    let mut parser = parser();
    report(&mut parser, "app=claude-code:state=blocked:kind=permission");
    report(&mut parser, "id=x:app=claude-code:state=working");
    let status = parser.program_status();
    let root = status.records().find(|record| record.id().is_empty()).expect("root record");
    assert_eq!(
        (root.state(), root.kind(), root.app()),
        (StatusState::Blocked, Some(StatusKind::Permission), "claude-code")
    );
    report(&mut parser, "state=clear:id=x");
    assert_eq!(records(&parser), vec![(String::new(), "blocked", Some("permission"), None)]);
}

/// `title` and `msg` decode from base64 (padding optional) within their encoded and decoded caps;
/// invalid base64, invalid UTF-8 or a control character discards the report.
#[test]
fn title_and_msg_decode_strictly_within_their_caps() {
    let mut parser = parser();
    let title = encode("Build".as_bytes());
    let unpadded = title.trim_end_matches('=').to_owned();
    report(&mut parser, &format!("state=done:title={unpadded}:msg={}", encode("ok ✓".as_bytes())));
    let status = parser.program_status();
    let record = status.records().next().expect("one record");
    assert_eq!((record.title(), record.msg()), ("Build", "ok ✓"));

    let longest_title = encode(&[b't'; 192]);
    report(&mut parser, &format!("state=idle:title={longest_title}"));
    assert_eq!(records(&parser)[0].1, "idle", "a 192-byte title fits");
    for bad in [
        format!("title={}", encode(&[b't'; 193])),
        format!("msg={}", encode(&[b'm'; 2049])),
        format!("msg={}", "A".repeat(2733)),
        format!("title={}", encode(&[0xff, 0xfe])),
        format!("title={}", encode(b"a\x07b")),
        format!("title={}", encode("a\u{85}b".as_bytes())),
        "title=QUJDR".to_owned(),
    ] {
        report(&mut parser, &format!("state=error:{bad}"));
        assert_eq!(records(&parser)[0].1, "idle", "{bad} discards");
    }
}

/// `app` over 32 bytes discards the report; one outside the name alphabet reads as empty.
#[test]
fn app_length_discards_and_bad_characters_empty_it() {
    let mut parser = parser();
    report(&mut parser, &format!("state=working:app={}", "a".repeat(32)));
    assert_eq!(parser.program_status().records().next().map(|record| record.app().len()), Some(32));
    report(&mut parser, &format!("state=done:app={}", "a".repeat(33)));
    assert_eq!(records(&parser)[0].1, "working", "a 33-byte app discards");
    report(&mut parser, "state=done:app=a,b");
    let status = parser.program_status();
    assert_eq!(
        status.records().next().map(|record| (record.state(), record.app())),
        Some((StatusState::Done, ""))
    );
}

/// `id` segments are `[A-Za-z0-9_.+-]`, at most 32 bytes each and 8 deep; a bad, empty or
/// over-long `id` discards the report, including on `clear`, while an omitted one is the root.
#[test]
fn id_rules_discard_rather_than_fall_back_to_the_root() {
    let mut parser = parser();
    let deepest = vec!["s".repeat(32); 8].join("/");
    report(&mut parser, &format!("id={deepest}:state=idle"));
    report(&mut parser, "state=working");
    assert_eq!(records(&parser).len(), 2);
    let too_deep = ["s"; 9].join("/");
    for bad_id in ["", "a//b", "/a", "a,b", &"s".repeat(33), &too_deep] {
        report(&mut parser, &format!("id={bad_id}:state=error"));
        report(&mut parser, &format!("id={bad_id}:state=clear"));
        assert_eq!(records(&parser).len(), 2, "{bad_id:?} discards");
    }
    report(&mut parser, "id=a b:state=error");
    assert_eq!(records(&parser).len(), 2, "a value outside the alphabet discards for id");
}

/// A key over 16 bytes discards the report; a key outside `[a-z]+`, a pair without `=` or a value
/// outside the alphabet is skipped; whitespace around a pair is trimmed and the last pair wins.
#[test]
fn keys_and_values_skip_or_discard_as_kitty_does() {
    let mut parser = parser();
    report(&mut parser, "state=working:abcdefghijklmnopq=1");
    assert!(parser.program_status().is_empty(), "a 17-byte key discards");
    report(
        &mut parser,
        "state=working:abcdefghijklmnop=1:Kind=x:bare:progress=5 0: state = blocked ",
    );
    assert_eq!(records(&parser), vec![(String::new(), "blocked", None, None)]);
    report(&mut parser, "state=idle:state=done");
    assert_eq!(records(&parser)[0].1, "done", "the last pair wins");
    report(&mut parser, &format!("msg={}:msg=QQ:state=error", "A".repeat(2733)));
    assert_eq!(records(&parser)[0].1, "done", "an earlier over-long msg still discards");
}

/// `clear` removes the record and its descendants only; a `clear` without `id` removes all.
#[test]
fn clear_removes_descendants_and_an_omitted_id_clears_all() {
    let mut parser = parser();
    for id in ["a", "a/b", "a/b/c", "ab", "z"] {
        report(&mut parser, &format!("id={id}:state=working"));
    }
    report(&mut parser, "id=a:state=clear");
    let ids: Vec<String> = records(&parser).into_iter().map(|record| record.0).collect();
    assert_eq!(ids, ["ab", "z"]);
    report(&mut parser, "state=clear");
    assert!(parser.program_status().is_empty());
}

/// A 33rd record evicts the least recently reported one; an identical report refreshes that
/// recency without changing the generation or the change order the tab marker breaks ties by.
#[test]
fn the_store_evicts_by_report_recency_and_identical_reports_change_nothing() {
    let mut parser = parser();
    for index in 0..MAX_STATUS_RECORDS {
        report(&mut parser, &format!("id=r{index}:state=working"));
    }
    let generation = parser.program_status().generation();
    let events = report(&mut parser, "id=r0:state=working");
    assert_eq!(parser.program_status().generation(), generation, "identical changes nothing");
    assert!(events.iter().all(|event| !matches!(event, VtEvent::ProgramStatus(_))));
    report(&mut parser, "id=new:state=working");
    let ids: Vec<String> = records(&parser).into_iter().map(|record| record.0).collect();
    assert_eq!(ids.len(), MAX_STATUS_RECORDS);
    assert!(ids.contains(&"r0".to_owned()), "the refreshed record survives");
    assert!(!ids.contains(&"r1".to_owned()), "the least recently reported is evicted");
    assert_eq!(ids.first().map(String::as_str), Some("r0"), "r0 kept its change position");
}

/// A batch with several changes publishes one summary event, the last state, with the most urgent
/// record (blocked over error over done over working over idle).
#[test]
fn one_summary_event_per_batch_names_the_most_urgent_record() {
    let mut parser = parser();
    let events = parser.advance(
        b"\x1b]7501;id=a:state=done\x07\x1b]7501;id=b:state=blocked\x07\x1b]7501;id=c:state=working:progress=7\x07",
    );
    let summaries: Vec<&StatusSummary> = events
        .iter()
        .filter_map(|event| match event {
            VtEvent::ProgramStatus(summary) => Some(summary),
            _ => None,
        })
        .collect();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].top.map(|top| top.state), Some(StatusState::Blocked));
    assert!(summaries[0].unseen, "a done record is unseen until acknowledged");
    assert_eq!(*summaries[0], parser.program_status_summary());
}

/// An admitted OSC 133 prompt start drops working, blocked and idle records at its position in the
/// stream, so a report after it in the same batch survives; done and error stay.
#[test]
fn a_prompt_start_drops_transient_records_in_stream_order() {
    let mut parser = parser();
    parser.advance(
        b"\x1b]7501;id=w:state=working\x07\x1b]7501;id=d:state=done\x07\x1b]133;A\x07\x1b]7501;id=n:state=blocked\x07",
    );
    let ids: Vec<String> = records(&parser).into_iter().map(|record| record.0).collect();
    assert_eq!(ids, ["d", "n"]);
}

/// Child exit drops transient records; acknowledgment (focus or a user key) drops done and error;
/// RIS drops everything. Each change moves the generation so a stale summary is never applied.
#[test]
fn exit_acknowledgment_and_ris_end_their_records() {
    let mut parser = parser();
    for (id, state) in
        [("w", "working"), ("b", "blocked"), ("i", "idle"), ("d", "done"), ("e", "error")]
    {
        report(&mut parser, &format!("id={id}:state={state}"));
    }
    let before = parser.program_status().generation();
    assert!(parser.end_transient_program_status());
    assert!(parser.program_status().generation() > before);
    let ids: Vec<String> = records(&parser).into_iter().map(|record| record.0).collect();
    assert_eq!(ids, ["d", "e"]);
    assert!(parser.acknowledge_program_status());
    assert!(parser.program_status().is_empty());
    assert!(!parser.acknowledge_program_status(), "nothing left to acknowledge");
    report(&mut parser, "state=working");
    let events = parser.advance(b"\x1bc");
    assert!(parser.program_status().is_empty(), "RIS removes every record");
    assert!(events
        .iter()
        .any(|event| matches!(event, VtEvent::ProgramStatus(summary) if summary.top.is_none())));
}

/// `process-exited` is kitty-private and reads as an invalid report, changing nothing.
#[test]
fn process_exited_is_ignored() {
    let mut parser = parser();
    report(&mut parser, "state=working");
    report(&mut parser, "process-exited");
    assert_eq!(records(&parser), vec![(String::new(), "working", None, None)]);
}

/// The store's `Debug` prints counts only, never ids, apps, titles or messages.
#[test]
fn debug_prints_no_payload() {
    let mut parser = parser();
    report(
        &mut parser,
        &format!("id=secret-id:app=secret-app:state=done:title={}", encode(b"secret")),
    );
    let printed =
        format!("{:?} {:?}", parser.program_status(), parser.program_status().records().next());
    assert!(!printed.contains("secret"), "{printed}");
}

/// `title` and `msg` padding is checked as supplied: canonical padding or none is accepted, and
/// a partial pad, excess `=` or a value of padding alone is invalid base64 that discards the
/// report.
#[test]
fn text_padding_is_validated_as_supplied() {
    let mut parser = parser();
    let padded = encode("Build".as_bytes());
    assert!(padded.ends_with('='), "the fixture carries canonical padding");
    report(&mut parser, &format!("state=done:title={padded}"));
    assert_eq!(records(&parser)[0].1, "done", "canonical padding is accepted");
    // `A` encodes as `QQ==`: unpadded `QQ` is accepted, a single `=` is a partial pad.
    report(&mut parser, "state=working:title=QQ");
    assert_eq!(records(&parser)[0].1, "working", "unpadded text is accepted");
    for bad in [
        "title=QQ=".to_owned(),
        "msg=QQ=".to_owned(),
        format!("title={padded}="),
        format!("msg={padded}=="),
        "title====".to_owned(),
        "msg==".to_owned(),
    ] {
        report(&mut parser, &format!("state=error:{bad}"));
        assert_eq!(records(&parser)[0].1, "working", "{bad} discards");
    }
}

/// The persistent program metadata figure counts what the records and user variables really own:
/// zero for a fresh parser, more than their text once full (vector slots and shared blocks), one
/// item per record and variable, never past `MAX_PROGRAM_METADATA_BYTES`, and back to the user
/// variables alone once RIS ends every record.
#[test]
fn program_metadata_counts_owned_allocations_within_its_cap() {
    let mut parser = parser();
    assert_eq!(parser.program_metadata_amount(), sonicterm_types::ResourceAmount::default());
    let (title, msg) = (encode(&[b't'; 192]), encode(&[b'm'; 2048]));
    for index in 0..MAX_STATUS_RECORDS {
        report(&mut parser, &format!("id=r{index}:app=tool:state=done:title={title}:msg={msg}"));
    }
    let value = encode(&[b'v'; 4000]);
    let vars: Vec<String> = (0..4).map(|index| format!("SetUserVar=n{index}={value}")).collect();
    parser.advance(format!("\x1b]1337;{}\x07", vars.join(";")).as_bytes());
    let text_bytes = MAX_STATUS_RECORDS * (192 + 2048) + 4 * 4002;
    let full = parser.program_metadata_amount();
    assert_eq!(full.items, MAX_STATUS_RECORDS + 4, "one item per record and variable");
    assert!(full.bytes > text_bytes, "{} bytes owned for {text_bytes} of text", full.bytes);
    assert!(full.bytes <= crate::vt::MAX_PROGRAM_METADATA_BYTES, "{} past the cap", full.bytes);
    assert_eq!(parser.retained_amount().bytes, 0, "none of it is capture");
    parser.advance(b"\x1bc");
    let after_reset = parser.program_metadata_amount();
    assert_eq!(after_reset.items, 4, "RIS ends the records and keeps the variables");
    assert!(after_reset.bytes < full.bytes);
}
