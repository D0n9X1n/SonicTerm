//! Pins when the pwsh integration parses a line: never for a successful one, and for a failed one
//! within its size and resolution caps, treating a line that defines, aliases or removes a command
//! as one that may run a native program. Each case reads D's exact code from a real PowerShell 7.

use std::process::Command;

use super::shell_integration_tests_pwsh_tests::{drive_pwsh, pwsh};
use super::*;

/// The pwsh to test, or `None` (after checking CI does not require it) to skip.
fn required_pwsh() -> Option<String> {
    let found = pwsh();
    if found.is_none() {
        // When: no pwsh is `found`, the test skips unless CI requires PowerShell.
        assert!(
            std::env::var_os("SONICTERM_REQUIRE_PWSH").is_none(),
            "SONICTERM_REQUIRE_PWSH is set but pwsh is not installed"
        );
        eprintln!("skipped: pwsh is not installed (set SONICTERM_TEST_PWSH)");
    }
    found
}

/// Each D's code in `output`, `None` for a D that carries none.
fn codes(output: &str) -> Vec<Option<i32>> {
    tokened(output)
        .iter()
        .filter_map(|marker| marker.strip_prefix('D'))
        .map(|rest| rest.strip_prefix(';').and_then(|rest| rest.split(';').next()?.parse().ok()))
        .collect()
}

/// `count` failing cmdlets on one line, each silenced, so the line fails with a fresh error record
/// and runs no native program.
fn cmdlets(count: usize) -> String {
    "gi /no/such -ea 0; ".repeat(count) + "\r"
}

/// Successful lines are never parsed. A failed line is: a function shadowing a cmdlet, a function
/// defined and called on the line, or a line past 64 commands may run native, so a fresh error
/// with a stale non-zero native code gives no code; 64 cmdlets are resolved and give 1.
#[test]
fn only_failed_lines_are_parsed_and_uncertain_ones_may_run_native() {
    let Some(pwsh) = required_pwsh() else { return };
    let sixty_four = cmdlets(64);
    let sixty_five = cmdlets(65);
    let lines: [&[u8]; 11] = [
        b"$null = 1\r",
        b"Get-Date > $null\r",
        b"Write-Output ('parses=' + $global:__sonicterm_parses + '.')\r",
        // A native failure leaves $LASTEXITCODE at 4 for every later line.
        b"sh -c 'exit 4'\r",
        // `Get-Item` resolves to this function now: shadowing is seen, so the line may run native.
        // Defining it is a failed line too: `$?` still holds the `sh` failure, and a line with a
        // definition may run native, so it reports the stale 4 rather than certify a cmdlet's 1.
        b"function global:Get-Item { sh -c 'exit 4' }\r",
        b"Get-Item x; Write-Error e\r",
        b"Remove-Item function:Get-Item\r",
        // Defined and called on one line, then removed: only the definition shows it.
        b"function f { sh -c 'exit 4' }; f; Remove-Item function:f; Write-Error e\r",
        sixty_four.as_bytes(),
        sixty_five.as_bytes(),
        b"Write-Output ('resolutions=' + $global:__sonicterm_resolutions + '.')\r",
    ];
    let output = drive_pwsh(&pwsh, "", Shell::Pwsh.snippet(), &lines);
    assert!(output.contains("parses=0."), "a successful line was parsed: {output:?}");
    assert_eq!(
        codes(&output),
        [Some(0), Some(0), Some(0), Some(4), Some(4), None, Some(0), None, Some(1), None, Some(0)],
        "the D codes: {:?}",
        tokened(&output)
    );
    assert!(output.contains("resolutions=64."), "the cap resolved more than 64: {output:?}");
}

/// The gate's cases, run after the snippet is loaded: each prints its name, whether the line may
/// run native, and how many parses it took.
const GATE_CASES: &str = r#"
$pad = { param($length) 'Get-Date' + ' ' * ($length - 8) }
$cases = [ordered]@{
  cmdlet = 'Get-Item x'
  define = 'function f { }; Get-Item x'
  scoped = 'if ($true) { function g { } }; Get-Item x'
  drive = '${function:h} = { }; Get-Item x'
  remove = 'Remove-Item function:Get-Item'
  alias = 'Set-Alias gx Get-Item'
  at_cap = & $pad 16384
  over_cap = & $pad 16385
}
foreach ($case in $cases.GetEnumerator()) {
  $before = $global:__sonicterm_parses
  $native = __sonicterm_may_run_native $case.Value
  "$($case.Key)=$native/$($global:__sonicterm_parses - $before)"
}
"#;

/// The parse gate itself: a definition, an alias, a function-drive write or removal is uncertain;
/// a plain cmdlet is not; a line of 16,384 UTF-16 code units is parsed and one more is not.
#[test]
fn the_parse_gate_is_conservative_and_bounded() {
    let Some(pwsh) = required_pwsh() else { return };
    // `-Command -` reads stdin line by line, which a here-string cannot span, so both scripts are
    // files: the snippet, and the cases that load it.
    let dir = std::env::temp_dir();
    let snippet = dir.join(format!("sonicterm-gate-{}-snippet.ps1", std::process::id()));
    let cases = dir.join(format!("sonicterm-gate-{}-cases.ps1", std::process::id()));
    std::fs::write(&snippet, Shell::Pwsh.snippet()).unwrap();
    let load = format!(
        "Import-Module PSReadLine; $env:SONICTERM_SHELL_TOKEN = 't'; . '{}'\n",
        snippet.display()
    );
    std::fs::write(&cases, load + GATE_CASES).unwrap();
    let output = Command::new(&pwsh)
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
        .arg(&cases)
        // A private startup cache: pwsh processes sharing one can leave it unreadable.
        .env("XDG_CACHE_HOME", dir.join(format!("sonicterm-gate-{}-cache", std::process::id())))
        .output()
        .expect("pwsh runs");
    let _ = std::fs::remove_file(&snippet);
    let _ = std::fs::remove_file(&cases);
    let _ =
        std::fs::remove_dir_all(dir.join(format!("sonicterm-gate-{}-cache", std::process::id())));
    let text = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "cmdlet=False/1",
        "define=True/1",
        "scoped=True/1",
        "drive=True/1",
        "remove=True/1",
        "alias=True/1",
        "at_cap=False/1",
        "over_cap=True/0",
    ] {
        assert!(
            text.contains(expected),
            "{expected} missing: {text} {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
