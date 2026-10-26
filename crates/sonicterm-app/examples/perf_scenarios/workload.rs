//! Generated role scripts, deterministic fixtures and the scratch configuration.
//!
//! Every pane's shell is one generated `/bin/sh` script. It claims the next role, starts the
//! cleanup anchor, records its session, waits for the acknowledgement and GO, runs its workload
//! and prints a completion sentinel. Fixtures come from a seeded generator, so both sides of a
//! comparison read the same bytes without any fixture file being stored in the repository.
//!
//! On Windows there is no `/bin/sh`, so the harness binary itself is every pane's shell:
//! `run_program` performs the same protocol and the same workload steps as the script.

use std::io::Write;
use std::path::Path;
#[cfg(any(windows, test))]
use std::time::Duration;

use crate::scenarios::{Fixture, Plan, Presentation, Workload};

/// The fixed prompt every idle shell shows; the probe waits for it before typing.
pub(crate) const PROMPT: &str = "perf$ ";
/// The `cmd.exe` `PROMPT` that renders as [`PROMPT`]: `$$` prints a dollar sign and `$S` a space.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const CMD_PROMPT: &str = "perf$$$S";
/// The variable the probe exports with the run's scratch directory; a pane's program reads it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const SCRATCH_ENV: &str = "SONICTERM_PERF_SCRATCH";
/// Configured grid width in cells; screen-sized fixtures are sized to it.
const GRID_COLS: usize = 250;
/// Configured grid height in cells.
const GRID_ROWS: usize = 70;
/// The scenario's text face, written out so a later default change cannot alter a scenario: the
/// platform's own monospace family, which every hosted runner has. Windows pins Consolas rather
/// than the Cascadia Mono default, because not every Windows image ships Cascadia Mono.
#[cfg(target_os = "macos")]
const FONT_FAMILY: &str = "Menlo";
/// The scenario's text face on Windows, present on every Windows release.
#[cfg(windows)]
const FONT_FAMILY: &str = "Consolas";
/// The scenario's text face elsewhere: the face fontconfig resolves for `monospace`.
#[cfg(not(any(target_os = "macos", windows)))]
const FONT_FAMILY: &str = "monospace";
/// Today's default font size in points, as TOML.
const FONT_SIZE: &str = "13.0";
/// How long the cleanup anchor outlives the run's own deadline, in seconds.
const ANCHOR_MARGIN_S: u64 = 300;
/// One fixed seed per generated fixture, so each one's bytes are pinned independently.
const BULK_SEED: u64 = 0x5045_5246_0001;
const DENSE_SEED: u64 = 0x5045_5246_0002;
const SEARCH_SEED: u64 = 0x5045_5246_0003;
const SCROLLBACK_SEED: u64 = 0x5045_5246_0004;
const HISTORY_SEED: u64 = 0x5045_5246_0005;
const EMOJI_SEED: u64 = 0x5045_5246_0006;
const FRAMES_SEED: u64 = 0x5045_5246_0007;
const LINKED_SEED: u64 = 0x5045_5246_0008;
/// The bulk fixture's repeated block: 8,192 lines of 128 bytes.
const BLOCK_BYTES: usize = 1 << 20;
const BULK_LINE_BYTES: usize = 128;
/// The bulk fixture's file name below `workload/fixtures/`.
const BULK_FILE_NAME: &str = "bulk.txt";
/// The URL S6/rest prints on its first row.
pub(crate) const REST_URL: &str = "https://example.com/sonicterm/rest";
/// The file S6/rest creates below `workload/` and prints the absolute path of on its second row.
pub(crate) const REST_TARGET_NAME: &str = "rest-target.txt";
/// Bytes per buffer `write_yes` writes: 32,768 `y` lines.
#[cfg_attr(not(test), allow(dead_code))]
const YES_BUFFER_BYTES: usize = 64 << 10;

/// The scratch `sonicterm.toml`: grid, font, log level, the role script as the shell, scrollback.
///
/// `laps` selects `debug`, which adds a `render_timing` line per frame, so laps runs are never
/// pooled with timed runs. The warm window pool keeps its default of one.
#[cfg(any(unix, test))]
pub(crate) fn config_toml(plan: &Plan, scratch: &str, laps: bool) -> String {
    config_toml_with_shell(plan, &format!("{scratch}/workload/role.sh"), laps)
}

/// The scratch `sonicterm.toml` with `shell` as every pane's program.
///
/// `shell` is written as a TOML literal string, so it must hold no `'` or control character;
/// a Windows path keeps its backslashes as they are.
pub(crate) fn config_toml_with_shell(plan: &Plan, shell: &str, laps: bool) -> String {
    let level = if laps { "debug" } else { "info" };
    let scrollback = plan.scrollback_rows;
    let rows = plan.window_rows.map_or(GRID_ROWS, usize::from);
    // A status-line run bounds presents by their causes, so the cursor must not blink there.
    let blink = if plan.status_line { "cursor_blink = false\n" } else { "" };
    let mut config = format!(
        "[window]\ncols = {GRID_COLS}\nrows = {rows}\n\n\
         [font]\nfamily = \"{FONT_FAMILY}\"\nsize = {FONT_SIZE}\n\n\
         [logging]\nlevel = \"{level}\"\n\n\
         [terminal]\nshell = '{shell}'\nscrollback = {scrollback}\n{blink}"
    );
    if plan.status_line {
        // A base without the status line ignores the table, so both sides run the same config.
        config.push_str("\n[status_line]\nenabled = true\n");
        if plan.scenario == "S1" {
            // S1 also lists `size`, so its output frames run with `history` and `size` both shown,
            // and `glyphs`, so the `glyphs` phase has a segment to show.
            config.push_str("right = [\"health\", \"glyphs\", \"history\", \"size\", \"mem\"]\n");
            // `held` stalls its reader for 1 s, so the threshold sits well inside that stall.
            config.push_str("\n[status_line.segment.input]\nheld_after_ms = 250\n");
        }
    }
    // Only the presenter variants set a mode, so every other config stays byte-identical.
    let mode = match plan.presentation {
        Presentation::Configured => return config,
        Presentation::ForceGdi => "force",
        Presentation::ForceWgpu => "off",
    };
    format!("{config}\n[appearance]\nsoftware_render_mode = \"{mode}\"\n")
}

/// The log filter a run needs in place of its configured level's, if any. `--laps` selects
/// Debug for `render_timing`, and Debug also admits `frame_counters`, which turns the App's
/// counter gate on; a laps run without `--counters` keeps Debug but turns that target off.
#[cfg(any(feature = "perf-counters", test))]
pub(crate) fn logging_filter(laps: bool, counters: bool) -> Option<String> {
    if !laps || counters {
        // When: `laps` is off (Info never opens the gate) or `counters` forces it on anyway.
        return None;
    }
    let debug = sonicterm_logging::filter_for_level(sonicterm_logging::LogLevel::Debug);
    let filter = if debug.contains("frame_counters=debug") {
        debug.replace("frame_counters=debug", "frame_counters=off")
    } else {
        // When: an older Debug filter has no `frame_counters` directive, it is added as off.
        format!("{debug},frame_counters=off")
    };
    Some(filter)
}

/// The `/bin/sh` script every pane runs as its shell.
///
/// It ignores its arguments (macOS adds login arguments), claims the next role with `mkdir`,
/// starts the cleanup anchor, records its session, waits for the acknowledgement and GO, then
/// runs its role's workload. The anchor is double-forked: launchd becomes its parent while it
/// stays in the leader's process group and session, ignores SIGHUP and holds no PTY descriptor,
/// so the session id cannot be reused while it lives.
#[cfg(any(unix, test))]
pub(crate) fn role_script(plan: &Plan, scratch: &str, nonce: &str) -> String {
    let bound_s = plan.timeout_s + ANCHOR_MARGIN_S;
    let short = if plan.short { " short" } else { "" };
    let (scenario, variant) = (plan.scenario, plan.variant);
    let mut script = String::new();
    for line in [
        "#!/bin/sh".to_owned(),
        format!("# perf_scenarios {scenario} {variant}{short}: generated role script; it ignores its arguments."),
        format!("scratch='{scratch}'"),
        "role=0".to_owned(),
        r#"while ! mkdir "$scratch/roles/$role" 2>/dev/null; do role=$((role + 1)); done"#.to_owned(),
        format!("anchor_pid=$( (trap '' HUP; exec sleep {bound_s}) </dev/null >/dev/null 2>&1 & echo $! )"),
        "tty_name=$(tty 2>/dev/null) || tty_name=none".to_owned(),
        r#"printf '{"role":%s,"leader_pid":%s,"anchor_pid":%s,"tty":"%s"}\n' "$role" "$$" "$anchor_pid" "$tty_name" > "$scratch/sessions/$role.json.tmp""#.to_owned(),
        r#"mv "$scratch/sessions/$role.json.tmp" "$scratch/sessions/$role.json""#.to_owned(),
        r#"while [ ! -e "$scratch/acks/$role" ]; do sleep 0.05; done"#.to_owned(),
        r#"printf 'READY %s\n' "$role""#.to_owned(),
        r#"if [ -p "$scratch/go/$role" ]; then read -r _go < "$scratch/go/$role"; else while [ ! -e "$scratch/go/$role" ]; do sleep 0.05; done; fi"#.to_owned(),
        r#"case "$role" in"#.to_owned(),
    ] {
        script.push_str(&line);
        script.push('\n');
    }
    for (role, workload) in plan.roles.iter().enumerate() {
        script.push_str(&format!("{role})\n"));
        for line in workload_lines(*workload, role, nonce, bound_s) {
            script.push_str("    ");
            script.push_str(&line);
            script.push('\n');
        }
        script.push_str("    ;;\n");
    }
    // An unplanned pane must neither print nor prompt, so it only sleeps out the bound.
    script.push_str(&format!("*)\n    exec sleep {bound_s}\n    ;;\nesac\n"));
    script
}

/// The shell lines that run `workload` as `role` after GO.
#[cfg(any(unix, test))]
fn workload_lines(workload: Workload, role: usize, nonce: &str, bound_s: u64) -> Vec<String> {
    let shell = vec![format!("export PS1='{PROMPT}'"), "exec /bin/zsh -f".to_owned()];
    let finish = vec![
        format!("printf '{}\\n'", sentinel_line(role, nonce)),
        format!(": > \"$scratch/done/{role}\""),
    ];
    let cat = |name: &str| format!("cat \"$scratch/workload/fixtures/{name}\"");
    match workload {
        Workload::IdleShell => shell,
        // role-exit runs only on Windows; the line keeps the script and the program's steps alike.
        Workload::ExitAfterGo => vec!["exit 1".to_owned()],
        Workload::Flood { lines, .. } => {
            [vec![format!("yes | head -n {lines}"), cat(BULK_FILE_NAME)], finish, shell].concat()
        }
        Workload::DateLoop => vec!["while :; do date; sleep 0.01; done".to_owned()],
        Workload::PrintThenShell(fixture) => {
            [vec![cat(fixture_file_name(fixture))], finish, shell].concat()
        }
        Workload::PrintThenSleep(fixture) => {
            let sleep = vec![format!("exec sleep {bound_s}")];
            [vec![cat(fixture_file_name(fixture))], finish, sleep].concat()
        }
        Workload::Frames { count, .. } => {
            // `sleep 0.016` paces about 60 frames a second; process start-up makes it a little slower.
            let play = format!(
                "while [ \"$frame\" -lt {count} ]; do cat \"$scratch/workload/fixtures/frames/$frame\"; sleep 0.016; frame=$((frame + 1)); done"
            );
            [vec!["frame=0".to_owned(), play], finish, shell].concat()
        }
        Workload::RestStream => [rest_fixture_lines(), finish, rest_region_lines()].concat(),
    }
}

/// S6/rest's fixture: clear the screen, create the rest target, print the URL and the target's
/// absolute path, then a blank row; the sentinel that follows lands on the fourth row.
fn rest_fixture_lines() -> Vec<String> {
    vec![
        r"printf '\033[H\033[2J'".to_owned(),
        format!(": > \"$scratch/workload/{REST_TARGET_NAME}\""),
        format!("printf '%s\\n' '{REST_URL}' \"$scratch/workload/{REST_TARGET_NAME}\" ''"),
    ]
}

/// S6/rest's stream: a scroll region from row `REST_REGION_TOP` down, the cursor at its bottom,
/// then a `date` line every 10 ms forever, so only rows inside the region change.
fn rest_region_lines() -> Vec<String> {
    let top = crate::scenarios::REST_REGION_TOP;
    vec![
        format!(r"printf '\033[{top}r\033[999;1H'"),
        "while :; do date; sleep 0.01; done".to_owned(),
    ]
}

/// The completion sentinel `role` prints after its workload.
pub(crate) fn sentinel_line(role: usize, nonce: &str) -> String {
    format!("PERF_DONE {role} {nonce}")
}

/// The line `role` prints once acknowledged, before it waits for GO.
pub(crate) fn ready_line(role: usize) -> String {
    format!("READY {role}")
}

/// A 16-digit hex nonce from `seed` that occurs in no fixture, so a sentinel match is unambiguous.
pub(crate) fn choose_nonce(seed: u64, fixtures: &[FixtureFile]) -> String {
    let mut rng = SeededRng::new(seed);
    loop {
        let candidate = format!("{:016x}", rng.next_u64());
        if !fixtures.iter().any(|file| file.contains(candidate.as_bytes())) {
            return candidate;
        }
    }
}

/// What `program.json` tells a pane's program: the plan to rebuild and the run's nonce.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgramSpec {
    /// Scenario id, as `scenarios::plan` takes it.
    pub(crate) scenario: String,
    /// Variant name.
    pub(crate) variant: String,
    /// Whether the run is `--short`.
    pub(crate) short: bool,
    /// The run's sentinel nonce.
    pub(crate) nonce: String,
}

/// The `program.json` document naming `plan` and `nonce`, from which a pane's program rebuilds both.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn program_json(plan: &Plan, nonce: &str) -> String {
    serde_json::json!({
        "scenario": plan.scenario,
        "variant": plan.variant,
        "short": plan.short,
        "nonce": nonce,
    })
    .to_string()
}

/// Reads a `program.json` document, refusing a missing or mistyped field with a reason naming it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_program_json(text: &str) -> Result<ProgramSpec, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("program.json is not JSON: {error}"))?;
    let object = value.as_object().ok_or_else(|| "program.json is not an object".to_owned())?;
    let text_field = |name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("program.json field `{name}` is missing or not a string"))
    };
    let short = object
        .get("short")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "program.json field `short` is missing or not a boolean".to_owned())?;
    Ok(ProgramSpec {
        scenario: text_field("scenario")?,
        variant: text_field("variant")?,
        short,
        nonce: text_field("nonce")?,
    })
}

/// One thing a pane's program does after GO; each step stands for one or two role-script lines.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProgramStep {
    /// Write this many `y` lines, as `yes | head -n` does.
    Yes(u32),
    /// Copy this file from `workload/fixtures/` to the output, as `cat` does.
    Cat(&'static str),
    /// Print a `date` line every 10 ms, never ending.
    DateLoop,
    /// Play this many frame files in order, each followed by a 16 ms sleep.
    Frames(u32),
    /// Print the completion sentinel.
    Sentinel,
    /// Create `done/<role>`.
    Done,
    /// Run the idle shell at the fixed prompt.
    Shell,
    /// Sleep out the run's bound, printing nothing.
    SleepBound,
    /// Exit 1 at once, printing nothing: `role-exit`'s role, which ends right after GO.
    ExitAfterGo,
    /// S6/rest's fixture: clear, create the rest target, print the URL, its path and a blank row.
    RestFixture,
    /// S6/rest's stream: set the scroll region, then print a `date` line every 10 ms forever.
    RestRegion,
}

/// The steps a pane's program performs for `workload`, in the order the role script runs them.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn program_steps(workload: Workload) -> Vec<ProgramStep> {
    use ProgramStep::{Cat, Done, Sentinel, Shell, SleepBound};
    match workload {
        Workload::IdleShell => vec![Shell],
        Workload::ExitAfterGo => vec![ProgramStep::ExitAfterGo],
        Workload::Flood { lines, .. } => {
            vec![ProgramStep::Yes(lines), Cat(BULK_FILE_NAME), Sentinel, Done, Shell]
        }
        Workload::DateLoop => vec![ProgramStep::DateLoop],
        Workload::PrintThenShell(fixture) => {
            vec![Cat(fixture_file_name(fixture)), Sentinel, Done, Shell]
        }
        Workload::PrintThenSleep(fixture) => {
            vec![Cat(fixture_file_name(fixture)), Sentinel, Done, SleepBound]
        }
        Workload::Frames { count, .. } => vec![ProgramStep::Frames(count), Sentinel, Done, Shell],
        Workload::RestStream => {
            vec![ProgramStep::RestFixture, Sentinel, Done, ProgramStep::RestRegion]
        }
    }
}

/// The role-script lines `step` stands for, written apart from `workload_lines` on purpose.
///
/// A test requires the two to agree for every plan, so either one drifting fails it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn posix_lines(
    step: &ProgramStep,
    role: usize,
    nonce: &str,
    bound_s: u64,
) -> Vec<String> {
    match *step {
        ProgramStep::Yes(lines) => vec![format!("yes | head -n {lines}")],
        ProgramStep::Cat(name) => vec![format!("cat \"$scratch/workload/fixtures/{name}\"")],
        ProgramStep::DateLoop => vec!["while :; do date; sleep 0.01; done".to_owned()],
        ProgramStep::Frames(count) => vec![
            "frame=0".to_owned(),
            format!(
                "while [ \"$frame\" -lt {count} ]; do cat \"$scratch/workload/fixtures/frames/$frame\"; sleep 0.016; frame=$((frame + 1)); done"
            ),
        ],
        ProgramStep::Sentinel => vec![format!("printf '{}\\n'", sentinel_line(role, nonce))],
        ProgramStep::Done => vec![format!(": > \"$scratch/done/{role}\"")],
        ProgramStep::Shell => vec![format!("export PS1='{PROMPT}'"), "exec /bin/zsh -f".to_owned()],
        ProgramStep::SleepBound => vec![format!("exec sleep {bound_s}")],
        ProgramStep::ExitAfterGo => vec!["exit 1".to_owned()],
        ProgramStep::RestFixture => rest_fixture_lines(),
        ProgramStep::RestRegion => rest_region_lines(),
    }
}

/// Writes `lines` lines of `y`, the bytes `yes | head -n <lines>` writes, in 64 KiB buffers.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_yes(out: &mut impl Write, lines: u32) -> std::io::Result<()> {
    let buffer = b"y\n".repeat(YES_BUFFER_BYTES / 2);
    // Counted in `u64`, so twice `u32::MAX` lines cannot overflow on any target.
    let mut remaining_bytes = 2 * u64::from(lines);
    while remaining_bytes > 0 {
        // Every chunk is even and at most one buffer, so it ends on a whole line.
        let chunk_bytes = remaining_bytes.min(YES_BUFFER_BYTES as u64);
        out.write_all(&buffer[..chunk_bytes as usize])?;
        remaining_bytes -= chunk_bytes;
    }
    Ok(())
}

/// The line POSIX `date` prints in the C locale for `unix_s`, in UTC: `Thu Jan  1 00:00:00 UTC 1970`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn date_line(unix_s: u64) -> String {
    // 1970-01-01, day zero, was a Thursday.
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let days = unix_s / 86_400;
    let second_of_day = unix_s % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{} {} {day:>2} {:02}:{:02}:{:02} UTC {year}",
        WEEKDAYS[(days % 7) as usize],
        MONTHS[month - 1],
        second_of_day / 3_600,
        second_of_day % 3_600 / 60,
        second_of_day % 60,
    )
}

/// The proleptic Gregorian `(year, month 1..=12, day 1..=31)` of `days` after 1970-01-01.
#[cfg_attr(not(test), allow(dead_code))]
fn civil_from_days(days: u64) -> (u64, usize, u64) {
    // Count from 0000-03-01, so each leap day ends its year and every era is 400 years long.
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    // Months counted from March: 0 is March and 11 is February.
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    // When: January and February belong to the calendar year after their March-based year began.
    let month = if march_month < 10 { march_month + 3 } else { march_month - 9 };
    let year = era * 400 + year_of_era + u64::from(month <= 2);
    (year, month as usize, day)
}

/// The session record a pane's program writes: its role, its process id and `tty` `none`.
///
/// A Windows program has no terminal device name, so `tty` holds the role script's own fallback.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn program_session_json(role: usize, program_pid: u32) -> String {
    serde_json::json!({ "role": role, "program_pid": program_pid, "tty": "none" }).to_string()
}


/// The three row-run workloads' screens: a fixed bold title on row 1, 68 patterned body rows
/// (rows 2-69) and a fixed normal footer on row 70, redrawn whole by every update.
///
/// Body rows mix bold, normal and italic segments; colour alternates per update but never
/// splits a run. Segments meant to shape carry CJK characters from the text face's fallback; the
/// changing fields meant not to shape are plain digits and spaces. Each body row ends after its
/// last segment, and the rest of the row is default-style blank cells. Only the tests use them
/// in this change; registering them as scenario variants comes with their diagnostic.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowRunWorkload {
    /// Repeated bold, normal and bold CJK segments, then an italic row and update number.
    Powerline,
    /// One of twelve CJK process names in bold, a normal CJK status, then an italic value.
    CjkTui,
    /// Four alternating bold and normal CJK segments unique to their row and update, then an
    /// italic row number.
    Unique,
}

/// The twelve process names `CjkTui` cycles through by row.
#[cfg(test)]
pub(crate) const CJK_TUI_NAMES: [&str; 12] = [
    "编译器",
    "数据库",
    "网络服务",
    "缓存",
    "调度器",
    "日志",
    "索引器",
    "监视器",
    "代理",
    "队列",
    "存储",
    "认证",
];

/// The first and last body rows, 1-based screen rows.
#[cfg(test)]
pub(crate) const ROW_RUN_BODY_ROWS: std::ops::RangeInclusive<u16> = 2..=69;

#[cfg(test)]
impl RowRunWorkload {
    /// The bytes of update `update`: every row rewritten in place, with SGR colour cycling per
    /// update. `update` is one counter that keeps counting across phases.
    pub(crate) fn update_bytes(self, update: u32) -> Vec<u8> {
        let colour = 31 + update % 6;
        let mut screen = String::new();
        screen.push_str(&format!("\x1b[1;1H\x1b[0;1m{}\x1b[0m\x1b[K", self.title()));
        for row in ROW_RUN_BODY_ROWS {
            screen.push_str(&format!("\x1b[{row};1H"));
            for (bold, italic, text) in self.body_segments(row, update) {
                let weight = if bold { ";1" } else { "" };
                let slant = if italic { ";3" } else { "" };
                screen.push_str(&format!("\x1b[0;{colour}{weight}{slant}m{text}"));
            }
            screen.push_str("\x1b[0m\x1b[K");
        }
        screen.push_str("\x1b[70;1H\x1b[0m按 q 退出\x1b[K");
        screen.into_bytes()
    }

    /// Row 1's fixed bold title.
    fn title(self) -> &'static str {
        match self {
            Self::Powerline => "提示符 工作负载",
            Self::CjkTui => "进程 工作负载",
            Self::Unique => "唯一 工作负载",
        }
    }

    /// Body row `row`'s segments for update `update`, each `(bold, italic, text)`.
    pub(crate) fn body_segments(self, row: u16, update: u32) -> Vec<(bool, bool, String)> {
        match self {
            Self::Powerline => vec![
                (true, false, "用户 主机 ".to_owned()),
                (false, false, "项目 终端 ".to_owned()),
                (true, false, "分支 主 ".to_owned()),
                (false, true, format!("{row:02} {update:06}")),
            ],
            Self::CjkTui => vec![
                (true, false, CJK_TUI_NAMES[usize::from(row) % 12].to_owned()),
                (false, false, " 运行中 ".to_owned()),
                (false, true, format!("{:05}", (update * 37 + u32::from(row)) % 100_000)),
            ],
            Self::Unique => {
                let mut segments: Vec<(bool, bool, String)> = (0..4)
                    .map(|segment| {
                        (segment % 2 == 0, false, format!("段{update:06}行{row:02}节{segment}"))
                    })
                    .collect();
                segments.push((false, true, format!("{row:02}")));
                segments
            }
        }
    }
}

#[cfg(test)]
#[path = "workload_tests.rs"]
mod workload_tests;

#[cfg(test)]
#[path = "glyph_oracle.rs"]
pub(crate) mod glyph_oracle;

#[path = "workload_fixtures.rs"]
mod fixtures;
#[path = "workload_native.rs"]
#[cfg(any(windows, test))]
mod native;
#[cfg(test)]
use fixtures::*;
pub(crate) use fixtures::{fixtures, FixtureBody, FixtureFile, SeededRng};
#[cfg(windows)]
pub(crate) use native::{connect_go, run_program};
#[cfg(any(windows, test))]
pub(crate) use native::ProgramHost;
#[cfg(windows)]
pub(crate) use native::run_steps;

use fixtures::fixture_file_name;
