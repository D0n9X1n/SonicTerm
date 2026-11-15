//! Pins `command_history` and `read_pane` `last_command` by `seq`: settled commands come newest
//! first within `limit`, each command's text is read from B to C (soft wraps joined), a missing B
//! gives no text, a D printed by a running program lists nothing, one cell budget is shared by
//! every entry, the primary list is read under the alternate screen, trust is reported per entry,
//! and a `seq` that is no longer retained is refused `evicted`.

use serde_json::{json, Value};

use super::super::read_reply;
use super::*;
use crate::app::mcp::read::MAX_CELLS_PER_READ;
use crate::app::mcp::test_entry;

/// Feed `bytes` to `entry`'s parser.
fn emit(entry: &PaneEntry, bytes: &[u8]) {
    entry.parser.lock().advance(bytes);
}

/// One shell command as an integrated shell prints it: prompt, typed `command`, its `output` and
/// D with `exit`. The next prompt is not printed, so the command is not settled yet.
fn run(entry: &PaneEntry, command: &str, output: &str, exit: i32) {
    let bytes = format!(
        "\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07{output}\r\n\x1b]133;D;{exit}\x07"
    );
    emit(entry, bytes.as_bytes());
}

/// The shell's next prompt, which settles the command before it.
fn prompt(entry: &PaneEntry) {
    emit(entry, b"\x1b]133;A\x07$ \x1b]133;B\x07");
}

/// The structured `command_history` result for `arguments`.
fn history(entry: &PaneEntry, arguments: &Value) -> Value {
    commands_reply(entry, arguments)["structuredContent"].clone()
}

/// The `command` field of every listed entry, newest first.
fn command_texts(result: &Value) -> Vec<Value> {
    result["commands"].as_array().unwrap().iter().map(|entry| entry["command"].clone()).collect()
}

/// Three commands and the next prompt list all three newest first with their codes, each with a
/// duration; `limit` keeps only the newest, and every listed `seq` reads that command's output.
#[test]
fn settled_commands_list_newest_first_and_read_by_seq() {
    let entry = test_entry(30, 20, 100);
    run(&entry, "make", "built", 0);
    run(&entry, "cargo test", "1 failed", 101);
    run(&entry, "lint", "clean", 0);
    prompt(&entry);
    let result = history(&entry, &json!({}));
    assert_eq!(command_texts(&result), vec![json!("lint"), json!("cargo test"), json!("make")]);
    let commands = result["commands"].as_array().unwrap();
    let codes: Vec<Value> = commands.iter().map(|entry| entry["exit_code"].clone()).collect();
    assert_eq!(codes, vec![json!(0), json!(101), json!(0)]);
    assert!(commands.iter().all(|entry| entry["duration_ms"].is_u64()), "{result}");
    assert!(commands.iter().all(|entry| entry["output_truncated"] == false));
    assert_eq!(result["partial"], false);

    let limited = history(&entry, &json!({ "limit": 2 }));
    assert_eq!(command_texts(&limited), vec![json!("lint"), json!("cargo test")]);
    for (listed, output) in commands.iter().zip(["clean", "1 failed", "built"]) {
        let read = read_reply(&entry, &json!({ "mode": "last_command", "seq": listed["seq"] }));
        assert_eq!(read["structuredContent"]["lines"], json!([output]), "{listed}");
        assert_eq!(read["structuredContent"]["exit_code"], listed["exit_code"]);
    }
}

/// A D printed by a still-running program does not list its command as finished; the shell's own
/// D and the next prompt do, with the shell's code.
#[test]
fn a_d_from_a_running_program_lists_nothing() {
    let entry = test_entry(30, 10, 100);
    run(&entry, "./tool", "\x1b]133;D;0\x07working", 0);
    assert_eq!(command_texts(&history(&entry, &json!({}))), Vec::<Value>::new());
    emit(&entry, b"\r\n\x1b]133;D;3\x07");
    prompt(&entry);
    let result = history(&entry, &json!({}));
    assert_eq!(command_texts(&result), vec![json!("./tool")]);
    assert_eq!(result["commands"][0]["exit_code"], 3);
}

/// A command whose C arrives on B's row is read from B's column to C's column; a region with no B
/// lists with a `null` command.
#[test]
fn b_and_c_on_one_row_and_no_b() {
    let entry = test_entry(30, 10, 100);
    emit(&entry, b"\x1b]133;A\x07$ \x1b]133;B\x07ls\x1b]133;C\x07\r\na.txt\r\n\x1b]133;D;0\x07");
    emit(&entry, b"\x1b]133;A\x07$ \x1b]133;C\x07\r\nno b\r\n\x1b]133;D;4\x07");
    prompt(&entry);
    let result = history(&entry, &json!({}));
    assert_eq!(command_texts(&result), vec![Value::Null, json!("ls")]);
    assert_eq!(result["commands"][0]["exit_code"], 4);
}

/// When B, C, the output and D all share one row, the command stops at C's column, so output
/// printed on that row after C never joins the command text.
#[test]
fn a_command_on_one_row_with_its_output_stops_at_c() {
    let entry = test_entry(40, 10, 100);
    emit(&entry, b"\x1b]133;A\x07$ \x1b]133;B\x07echo ok\x1b]133;C\x07RESULT\x1b]133;D;0\x07");
    emit(&entry, b"\x1b]133;A\x07");
    assert_eq!(command_texts(&history(&entry, &json!({}))), vec![json!("echo ok")]);
}

/// A typed command that soft-wraps is joined into one command, keeping the spaces at the wrap.
#[test]
fn a_command_across_a_soft_wrap_is_joined() {
    let entry = test_entry(10, 10, 100);
    // "$ " plus "echo ab " fills the 10-column row; " yz" continues on the wrapped row.
    run(&entry, "echo ab  yz", "ab  yz", 0);
    prompt(&entry);
    assert_eq!(command_texts(&history(&entry, &json!({}))), vec![json!("echo ab  yz")]);
}

/// Every entry's copy spends one shared cell budget: once it runs out the older entries come back
/// without `command`, still with their codes, and the result says `partial`.
#[test]
fn the_shared_cell_budget_sets_partial() {
    let cols = 1_000usize;
    let entry = test_entry(cols as u16, 10, 1_000);
    for index in 0..40 {
        run(&entry, &format!("cmd {index}"), "out", 0);
    }
    prompt(&entry);
    let result = history(&entry, &json!({ "limit": 50 }));
    let texts = command_texts(&result);
    assert_eq!(texts.len(), 40);
    // Each command's one input row costs a full row of cells.
    let copied = MAX_CELLS_PER_READ / cols;
    assert!(texts[..copied].iter().all(Value::is_string), "{texts:?}");
    assert!(texts[copied..].iter().all(Value::is_null), "{texts:?}");
    assert_eq!(texts[0], json!("cmd 39"));
    assert!(result["commands"][39]["exit_code"] == 0);
    assert_eq!(result["partial"], true);
}

/// The primary screen's commands are listed while a full-screen program holds the alternate
/// screen, with the text the primary screen holds; the running program itself is not listed.
#[test]
fn commands_are_listed_under_the_alternate_screen() {
    let entry = test_entry(30, 10, 100);
    run(&entry, "git log", "commit", 0);
    prompt(&entry);
    emit(&entry, b"vim\r\n\x1b]133;C\x07\x1b[?1049hfull screen");
    let result = history(&entry, &json!({}));
    assert_eq!(command_texts(&result), vec![json!("git log")]);
    assert_eq!(result["commands"][0]["exit_code"], 0);
}

/// Each entry reports whether its latest D carried the pane's shell token: a command run before
/// the integration authenticated is `false`, later ones are `true`.
#[test]
fn trust_is_reported_per_entry() {
    let token = "00112233445566778899aabbccddeeff";
    let entry = test_entry(40, 20, 100);
    entry.parser.lock().set_shell_token(Some(token.to_owned()));
    run(&entry, "early", "x", 0);
    let marker = |kind: &str| format!("\x1b]133;{kind};sonicterm_token={token}\x07");
    let tokened = format!(
        "{}$ {}later\r\n{}y\r\n{}{}$ ",
        marker("A"),
        marker("B"),
        marker("C"),
        marker("D;0"),
        marker("A"),
    );
    emit(&entry, tokened.as_bytes());
    let result = history(&entry, &json!({}));
    assert_eq!(command_texts(&result), vec![json!("later"), json!("early")]);
    assert_eq!(result["commands"][0]["exit_authenticated"], true);
    assert_eq!(result["commands"][1]["exit_authenticated"], false);
}

/// A `seq` whose region is no longer retained is refused `evicted`; a retained one whose command
/// has not finished reads as `no_command`; `seq` outside `last_command` or not an integer is
/// refused as invalid.
#[test]
fn an_unretained_seq_is_evicted() {
    let entry = test_entry(20, 4, 3);
    for index in 0..5 {
        run(&entry, &format!("c{index}"), "out", 0);
    }
    prompt(&entry);
    let refusal = |reply: Value| reply["structuredContent"]["error"].as_str().map(str::to_owned);
    let evicted = json!({ "mode": "last_command", "seq": 0 });
    assert_eq!(refusal(read_reply(&entry, &evicted)).as_deref(), Some("evicted"));
    let open = json!({ "mode": "last_command", "seq": 5 });
    let reply = read_reply(&entry, &open);
    assert_eq!(refusal(reply.clone()), None, "{reply}");
    assert_eq!(reply["structuredContent"]["no_command"], true);
    let tail = json!({ "mode": "tail", "seq": 1 });
    assert_eq!(refusal(read_reply(&entry, &tail)).as_deref(), Some("invalid_arguments"));
    let text = json!({ "mode": "last_command", "seq": "one" });
    assert_eq!(refusal(read_reply(&entry, &text)).as_deref(), Some("invalid_arguments"));
}

/// A held parser answers `busy` and `partial` with no entries; `limit` is clamped to 1 through 50.
#[test]
fn a_busy_parser_and_limit_clamping() {
    let entry = test_entry(20, 4, 100);
    let held = entry.parser.lock();
    let result = history(&entry, &json!({}));
    drop(held);
    assert_eq!((result["busy"].clone(), result["partial"].clone()), (json!(true), json!(true)));
    assert_eq!(command_texts(&result), Vec::<Value>::new());
    assert_eq!(limit_arg(&json!({})), DEFAULT_COMMANDS);
    assert_eq!(limit_arg(&json!({ "limit": 0 })), 1);
    assert_eq!(limit_arg(&json!({ "limit": 9_999 })), MAX_COMMANDS);
}
