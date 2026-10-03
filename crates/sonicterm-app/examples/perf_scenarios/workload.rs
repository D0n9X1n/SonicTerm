//! Generated role scripts, deterministic fixtures and the scratch configuration.
//!
//! Every pane's shell is one generated `/bin/sh` script. It claims the next role, starts the
//! cleanup anchor, records its session, waits for the acknowledgement and GO, runs its workload
//! and prints a completion sentinel. Fixtures come from a seeded generator, so both sides of a
//! comparison read the same bytes without any fixture file being stored in the repository.

use std::io::Write;
use std::path::Path;

use crate::scenarios::{Fixture, Plan, Workload};

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
/// Today's `FontConfig` defaults, written out so a later default change cannot alter a scenario.
const FONT_FAMILY: &str = "Rec Mono St.Helens";
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
/// The bulk fixture's repeated block: 8,192 lines of 128 bytes.
const BLOCK_BYTES: usize = 1 << 20;
const BULK_LINE_BYTES: usize = 128;
/// The bulk fixture's file name below `workload/fixtures/`.
const BULK_FILE_NAME: &str = "bulk.txt";
/// Bytes per buffer `write_yes` writes: 32,768 `y` lines.
#[cfg_attr(not(test), allow(dead_code))]
const YES_BUFFER_BYTES: usize = 64 << 10;

/// The scratch `sonicterm.toml`: grid, font, log level, the role script as the shell, scrollback.
///
/// `laps` selects `debug`, which adds a `render_timing` line per frame, so laps runs are never
/// pooled with timed runs. The warm window pool keeps its default of one.
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
    format!(
        "[window]\ncols = {GRID_COLS}\nrows = {GRID_ROWS}\n\n\
         [font]\nfamily = \"{FONT_FAMILY}\"\nsize = {FONT_SIZE}\n\n\
         [logging]\nlevel = \"{level}\"\n\n\
         [terminal]\nshell = '{shell}'\nscrollback = {scrollback}\n"
    )
}

/// SplitMix64: a small seeded generator, so fixtures need no new dependency.
pub(crate) struct SeededRng(u64);

impl SeededRng {
    /// A generator that starts from `seed`.
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next 64 bits of the SplitMix64 sequence.
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// A value in `0..bound`; the modulo bias is irrelevant for fixture text.
    pub(crate) fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound.max(1) as u64) as usize
    }
}

/// Lowercase vocabulary for generated text; none of it spells a protocol line.
const WORDS: &[&str] = &[
    "alpha", "branch", "cargo", "delta", "event", "frame", "glyph", "harbor", "index", "kernel",
    "layout", "matrix", "native", "orbit", "packet", "quartz", "render", "socket", "thread",
    "update", "vertex", "window", "yield", "zephyr", "buffer", "cursor", "driver", "export",
    "filter", "gamma", "height", "inline", "joiner", "keeper", "ledger", "marker",
];
/// Words with many `e`, so S8's query matches densely.
const E_WORDS: &[&str] = &[
    "eleven",
    "freeze",
    "tree",
    "beekeeper",
    "referee",
    "sleeve",
    "between",
    "eerie",
    "ember",
    "settee",
    "cheese",
    "geese",
    "reverence",
    "emcee",
    "seventeen",
    "esteem",
];
/// Wide emoji and CJK tokens for S9, including modifier, ZWJ and flag sequences.
const WIDE_TOKENS: &[&str] = &[
    "😀",
    "🚀",
    "🌏",
    "🍣",
    "👍🏽",
    "👨‍👩‍👧‍👦",
    "🇯🇵",
    "漢字",
    "中文",
    "日本語",
    "한국어",
    "東京",
    "ありがとう",
    "カタカナ",
    "表示",
    "测试",
];
/// Commands the htop-like table cycles through.
const COMMANDS: &[&str] = &[
    "zsh",
    "sonicterm",
    "cargo build",
    "rustc",
    "WindowServer",
    "kernel_task",
    "mds_stores",
    "launchd",
    "sshd",
    "node",
    "python3",
    "vim",
];

/// One line of seeded words after `prefix`, exactly `width` bytes; every word is ASCII.
fn word_line(rng: &mut SeededRng, words: &[&str], prefix: &str, width: usize) -> String {
    let mut line = String::with_capacity(width + 16);
    line.push_str(prefix);
    while line.len() < width {
        line.push_str(words[rng.below(words.len())]);
        line.push(' ');
    }
    line.truncate(width);
    line
}

/// The S3 fixture: one seeded 1 MiB block of 127-column lines, repeated to `bulk_bytes`.
fn bulk_fixture(bulk_bytes: u64) -> FixtureFile {
    let mut rng = SeededRng::new(BULK_SEED);
    let mut block = Vec::with_capacity(BLOCK_BYTES);
    while block.len() < BLOCK_BYTES {
        block.extend_from_slice(word_line(&mut rng, WORDS, "", BULK_LINE_BYTES - 1).as_bytes());
        block.push(b'\n');
    }
    let count = usize::try_from(bulk_bytes).unwrap_or(usize::MAX) / BLOCK_BYTES;
    FixtureFile {
        relative_path: BULK_FILE_NAME.to_owned(),
        body: FixtureBody::Repeated { block, count },
    }
}

/// One screen of seeded words: `GRID_ROWS` lines one cell narrower than the grid, so none wraps.
fn screen_lines(rng: &mut SeededRng, words: &[&str], text: &mut String) {
    for _ in 0..GRID_ROWS {
        text.push_str(&word_line(rng, words, "", GRID_COLS - 1));
        text.push('\n');
    }
}

/// `count` numbered 79-column lines of seeded words.
fn numbered_lines(rng: &mut SeededRng, count: usize, text: &mut String) {
    for number in 1..=count {
        text.push_str(&word_line(rng, WORDS, &format!("{number:05} "), 79));
        text.push('\n');
    }
}

/// 200 lines mixing wide emoji and CJK tokens with ASCII words, at most about 110 cells each.
fn emoji_cjk_lines(text: &mut String) {
    let mut rng = SeededRng::new(EMOJI_SEED);
    for _ in 0..200 {
        let mut cells = 0;
        while cells < 96 {
            if rng.below(2) == 0 {
                let token = WIDE_TOKENS[rng.below(WIDE_TOKENS.len())];
                text.push_str(token);
                // Two cells per scalar over-counts joined sequences, which keeps lines short.
                cells += 2 * token.chars().count();
            } else {
                let word = WORDS[rng.below(WORDS.len())];
                text.push_str(word);
                cells += word.len();
            }
            text.push(' ');
            cells += 1;
        }
        text.push('\n');
    }
}

/// One Sixel image, 480 × 240 pixels of colored bands, then a newline below it.
fn sixel_image() -> Vec<u8> {
    let mut image = String::from("\x1bPq\"1;1;480;240#0;2;90;20;20#1;2;20;80;30#2;2;20;30;90");
    for band in 0..40 {
        image.push_str(&format!("#{}!480~-", band % 3));
    }
    image.push_str("\x1b\\\n");
    image.into_bytes()
}

/// The generated bytes of a single-file fixture.
fn fixture_bytes(fixture: Fixture) -> Vec<u8> {
    let mut text = String::new();
    match fixture {
        Fixture::DenseScreen => screen_lines(&mut SeededRng::new(DENSE_SEED), WORDS, &mut text),
        Fixture::SearchText => screen_lines(&mut SeededRng::new(SEARCH_SEED), E_WORDS, &mut text),
        Fixture::ScrollbackLines => {
            numbered_lines(&mut SeededRng::new(SCROLLBACK_SEED), 12_000, &mut text);
        }
        Fixture::HistoryScreen => {
            let mut rng = SeededRng::new(HISTORY_SEED);
            numbered_lines(&mut rng, 1_000, &mut text);
            screen_lines(&mut rng, WORDS, &mut text);
        }
        Fixture::EmojiCjk => emoji_cjk_lines(&mut text),
        Fixture::Sixel => return sixel_image(),
    }
    text.into_bytes()
}

/// Rows the vim-like view keeps for text; the last grid row is its status line.
const VIM_TEXT_ROWS: usize = GRID_ROWS - 1;

/// `count` full-screen frames: the first half scroll a vim-like view one line a frame, the second
/// half redraw an htop-like table. The first frame enters the alternate screen with autowrap off
/// and the last restores both; `synchronized` wraps every frame in DEC 2026 brackets.
fn frame_fixtures(count: u32, synchronized: bool) -> Vec<FixtureFile> {
    let mut rng = SeededRng::new(FRAMES_SEED);
    let count = count as usize;
    let vim_frames = count / 2;
    (0..count)
        .map(|index| {
            let mut frame = String::new();
            if index == 0 {
                frame.push_str("\x1b[?1049h\x1b[?7l\x1b[2J");
                vim_screen(&mut rng, &mut frame);
            } else if index < vim_frames {
                vim_scroll(&mut rng, index, &mut frame);
            } else {
                htop_screen(&mut rng, index, &mut frame);
            }
            if index + 1 == count {
                frame.push_str("\x1b[?7h\x1b[?1049l");
            }
            let mut bytes = Vec::with_capacity(frame.len() + 16);
            if synchronized {
                bytes.extend_from_slice(b"\x1b[?2026h");
            }
            bytes.extend_from_slice(frame.as_bytes());
            if synchronized {
                bytes.extend_from_slice(b"\x1b[?2026l");
            }
            FixtureFile {
                relative_path: format!("frames/{index}"),
                body: FixtureBody::Bytes(bytes),
            }
        })
        .collect()
}

/// Paint every text row of the vim-like view, then its status line.
fn vim_screen(rng: &mut SeededRng, frame: &mut String) {
    for row in 1..=VIM_TEXT_ROWS {
        frame.push_str(&format!("\x1b[{row};1H"));
        frame.push_str(&word_line(rng, WORDS, &format!("{row:>5} "), GRID_COLS - 1));
    }
    vim_status(1, frame);
}

/// Scroll the text rows up one line inside a scroll region, as vim does, and write the new line.
fn vim_scroll(rng: &mut SeededRng, index: usize, frame: &mut String) {
    let line_number = VIM_TEXT_ROWS + index;
    // A line feed at the region's bottom margin scrolls only the text rows; the status line stays.
    frame.push_str(&format!("\x1b[1;{VIM_TEXT_ROWS}r\x1b[{VIM_TEXT_ROWS};1H\n\x1b[r"));
    frame.push_str(&format!("\x1b[{VIM_TEXT_ROWS};1H\x1b[2K"));
    frame.push_str(&word_line(rng, WORDS, &format!("{line_number:>5} "), GRID_COLS - 1));
    vim_status(index + 1, frame);
}

/// The reverse-video status line on the bottom row.
fn vim_status(top_line: usize, frame: &mut String) {
    frame.push_str(&format!(
        "\x1b[{GRID_ROWS};1H\x1b[7m\x1b[2K\"fixture.txt\" line {top_line} of 99999\x1b[0m"
    ));
}

/// Redraw an htop-like table: four CPU meters, a summary, a header and one process per row.
fn htop_screen(rng: &mut SeededRng, index: usize, frame: &mut String) {
    for cpu in 0..4 {
        let load = rng.below(101);
        let filled = load * 40 / 100;
        frame.push_str(&format!(
            "\x1b[{};1H  {cpu} [\x1b[32m{}\x1b[0m{}{load:>3}%]\x1b[K",
            cpu + 1,
            "|".repeat(filled),
            " ".repeat(40 - filled),
        ));
    }
    frame.push_str(&format!(
        "\x1b[5;1H  Tasks: {} total; load {}.{:02}\x1b[K\x1b[6;1H  Uptime frame {index}\x1b[K",
        200 + rng.below(50),
        rng.below(8),
        rng.below(100),
    ));
    frame.push_str(
        "\x1b[7;1H\x1b[30;42m    PID USER       PRI  NI  VIRT   RES S CPU% MEM%    TIME+  Command\x1b[K\x1b[0m",
    );
    for row in 8..=GRID_ROWS {
        let command = COMMANDS[rng.below(COMMANDS.len())];
        frame.push_str(&format!(
            "\x1b[{row};1H{:>7} perf       {:>3} {:>3} {:>5}M {:>5}M S {:>4} {:>4} {:>3}:{:02}.{:02} {command}\x1b[K",
            100 + rng.below(90_000),
            rng.below(40),
            rng.below(20),
            rng.below(9_000),
            rng.below(900),
            rng.below(100),
            rng.below(100),
            rng.below(60),
            rng.below(60),
            rng.below(100),
        ));
    }
}

/// A fixture's bytes; a large one repeats one block instead of holding every copy.
#[derive(Clone, Debug)]
pub(crate) enum FixtureBody {
    /// The whole file.
    Bytes(Vec<u8>),
    /// `block` written `count` times.
    Repeated { block: Vec<u8>, count: usize },
}

/// One file under `workload/fixtures/`.
#[derive(Clone, Debug)]
pub(crate) struct FixtureFile {
    /// Path below `workload/fixtures/`, with `/` separators.
    pub(crate) relative_path: String,
    /// The file's bytes.
    pub(crate) body: FixtureBody,
}

impl FixtureFile {
    /// The file's length in bytes.
    pub(crate) fn byte_len(&self) -> usize {
        match &self.body {
            FixtureBody::Bytes(bytes) => bytes.len(),
            FixtureBody::Repeated { block, count } => block.len() * count,
        }
    }

    /// Whether `needle` occurs anywhere in the file, including across a repeated block's seam.
    pub(crate) fn contains(&self, needle: &[u8]) -> bool {
        match &self.body {
            FixtureBody::Bytes(bytes) => contains_bytes(bytes, needle),
            FixtureBody::Repeated { block, count } => {
                if block.is_empty() || *count == 0 {
                    // When: the repeated body is empty, only the empty needle occurs in it.
                    return needle.is_empty();
                }
                // Enough copies that every window of `needle.len()` starting in the first copy fits.
                let copies = (needle.len().div_ceil(block.len()) + 1).min(*count);
                contains_bytes(&block.repeat(copies), needle)
            }
        }
    }

    /// Write the file below `root`, creating its parent directories.
    pub(crate) fn write_under(&self, root: &Path) -> std::io::Result<()> {
        let path = root.join(&self.relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
        match &self.body {
            FixtureBody::Bytes(bytes) => file.write_all(bytes)?,
            FixtureBody::Repeated { block, count } => {
                for _ in 0..*count {
                    file.write_all(block)?;
                }
            }
        }
        file.flush()
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|window| window == needle)
}

/// Every fixture file `plan`'s roles read, each written once.
pub(crate) fn fixtures(plan: &Plan) -> Vec<FixtureFile> {
    let mut files: Vec<FixtureFile> = Vec::new();
    for (index, workload) in plan.roles.iter().enumerate() {
        if plan.roles[..index].contains(workload) {
            // When: an earlier role runs the same workload, its fixture already exists.
            continue;
        }
        match *workload {
            Workload::IdleShell | Workload::DateLoop => {}
            Workload::Flood { bulk_bytes, .. } => files.push(bulk_fixture(bulk_bytes)),
            Workload::PrintThenShell(fixture) | Workload::PrintThenSleep(fixture) => {
                let relative_path = fixture_file_name(fixture).to_owned();
                if !files.iter().any(|file| file.relative_path == relative_path) {
                    let body = FixtureBody::Bytes(fixture_bytes(fixture));
                    files.push(FixtureFile { relative_path, body });
                }
            }
            Workload::Frames { count, synchronized } => {
                files.extend(frame_fixtures(count, synchronized));
            }
        }
    }
    files
}

/// The file a single-file fixture is written to.
fn fixture_file_name(fixture: Fixture) -> &'static str {
    match fixture {
        Fixture::DenseScreen => "dense.txt",
        Fixture::SearchText => "search.txt",
        Fixture::ScrollbackLines => "scrollback.txt",
        Fixture::HistoryScreen => "history.txt",
        Fixture::EmojiCjk => "emoji-cjk.txt",
        Fixture::Sixel => "image.sixel",
    }
}

/// The `/bin/sh` script every pane runs as its shell.
///
/// It ignores its arguments (macOS adds login arguments), claims the next role with `mkdir`,
/// starts the cleanup anchor, records its session, waits for the acknowledgement and GO, then
/// runs its role's workload. The anchor is double-forked: launchd becomes its parent while it
/// stays in the leader's process group and session, ignores SIGHUP and holds no PTY descriptor,
/// so the session id cannot be reused while it lives.
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
        r#"while [ ! -e "$scratch/go/$role" ]; do sleep 0.05; done"#.to_owned(),
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
fn workload_lines(workload: Workload, role: usize, nonce: &str, bound_s: u64) -> Vec<String> {
    let shell = vec![format!("export PS1='{PROMPT}'"), "exec /bin/zsh -f".to_owned()];
    let finish = vec![
        format!("printf '{}\\n'", sentinel_line(role, nonce)),
        format!(": > \"$scratch/done/{role}\""),
    ];
    let cat = |name: &str| format!("cat \"$scratch/workload/fixtures/{name}\"");
    match workload {
        Workload::IdleShell => shell,
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
    }
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
}

/// The steps a pane's program performs for `workload`, in the order the role script runs them.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn program_steps(workload: Workload) -> Vec<ProgramStep> {
    use ProgramStep::{Cat, Done, Sentinel, Shell, SleepBound};
    match workload {
        Workload::IdleShell => vec![Shell],
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

#[cfg(test)]
#[path = "workload_tests.rs"]
mod workload_tests;
