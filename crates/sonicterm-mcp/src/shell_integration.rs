//! The first-party OSC 133 shell integration `sonicterm-mcp --shell-integration <shell>` prints.
//!
//! A pane's shell gets a fresh secret in `SONICTERM_SHELL_TOKEN`. The snippet, evaluated by the
//! user's interactive shell, reads it once, removes it from the environment so no command the
//! shell runs inherits it, and appends it to every OSC 133 prompt, command and exit marker. A
//! marker carrying the pane's token is the shell's own: SonicTerm then verifies `run_command`'s
//! exit code and drops tokenless markers, so a program's output can neither forge a completion
//! nor downgrade the pane. The text is fixed: the token is read at run time, never written into
//! the snippet, so the output is the same in every pane.

/// The shells the snippet supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// zsh 5.3 or newer (`add-zsh-hook`, `add-zle-hook-widget`).
    Zsh,
    /// bash 4.4 or newer (`PS0`).
    Bash,
    /// PowerShell 7 or newer with PSReadLine.
    Pwsh,
}

impl Shell {
    /// The shell named `name`, as given on the command line.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            "pwsh" | "powershell" => Some(Self::Pwsh),
            _ => None,
        }
    }

    /// The snippet for this shell.
    pub fn snippet(self) -> &'static str {
        match self {
            Self::Zsh => ZSH,
            Self::Bash => BASH,
            Self::Pwsh => PWSH,
        }
    }
}

/// Every snippet's D names where its code came from with `sonicterm_src=`, SonicTerm's own key:
/// `status` for zsh's and bash's `$?` and PowerShell's success, `native` and `powershell` for
/// PowerShell's two failure readings; a D with no code names none.
///
/// zsh: D and A from `precmd` (D first, carrying the status the command left), B once the line
/// editor starts, C from `preexec`, carrying the line `preexec` was given as `cmdline_url=`,
/// percent-encoded byte by byte, when it is at most 1 KiB. Hooks are added beside any existing ones, never replacing them.
/// The token lives only in `__sonicterm_token`: unset first, so no export attribute set on that
/// name earlier survives, then declared inside a function with `allexport` off (zsh exports a
/// `typeset` assignment under `allexport` even with `+x`), so no command inherits it.
const ZSH: &str = r#"# SonicTerm shell integration (zsh). Load with: eval "$(sonicterm-mcp --shell-integration zsh)"
if [[ -o interactive && -n "${SONICTERM_SHELL_TOKEN-}" && -z "${__sonicterm_token-}" ]]; then
  unset __sonicterm_token __sonicterm_ran
  () {
    setopt localoptions noallexport
    typeset -g +x __sonicterm_token="$SONICTERM_SHELL_TOKEN"
    typeset -g +x -i __sonicterm_ran=0
  }
  unset SONICTERM_SHELL_TOKEN
  __sonicterm_mark() { printf '\e]133;%s;sonicterm_token=%s\a' "$1" "$__sonicterm_token"; }
  __sonicterm_precmd() {
    local __sonicterm_status=$?
    if (( __sonicterm_ran )); then __sonicterm_mark "D;$__sonicterm_status;sonicterm_src=status"; fi
    __sonicterm_ran=0
    __sonicterm_mark A
    return $__sonicterm_status
  }
  __sonicterm_preexec() {
    __sonicterm_ran=1
    emulate -L zsh
    setopt no_multibyte
    local text=$1 url= char hex index
    if (( ${#text} <= 1024 )); then
      for (( index = 1; index <= ${#text}; index++ )); do
        char=${text[index]}
        case $char in
          [A-Za-z0-9._~-]) url+=$char ;;
          *) printf -v hex '%%%02X' "'$char"; url+=$hex ;;
        esac
      done
    fi
    if [[ -n $url ]]; then __sonicterm_mark "C;cmdline_url=$url"; else __sonicterm_mark C; fi
  }
  __sonicterm_line_init() { __sonicterm_mark B; }
  autoload -Uz add-zsh-hook add-zle-hook-widget
  add-zsh-hook precmd __sonicterm_precmd
  add-zsh-hook preexec __sonicterm_preexec
  add-zle-hook-widget line-init __sonicterm_line_init
fi
"#;

/// bash: D and A from `PROMPT_COMMAND` (first, so it sees the command's status, which it
/// restores), B at the end of `PS1`, C from `PS0` as a command starts. Whether a command ran is a
/// flag `PS0` sets as bash shows it, through an arithmetic assignment inside a parameter expansion,
/// which prompt expansion runs in the shell itself; bash shows `PS0` before every command it runs
/// and never for an empty line, whatever the history settings. The prompt's command number (`\#`)
/// cannot tell: once `PS0` is set, bash 5.3 advances it lazily, so the first command after the
/// snippet loads leaves it unchanged. The token and the B marker that carries it are unexported
/// (`export -n`), so even `set -a` cannot hand them to a command, and `PS1` refers to the marker
/// by name rather than holding it. C carries the command from history as `cmdline_url=` only when
/// history recorded it (`HISTCMD` passed its value at the prompt), so a line history ignored never
/// carries an older command's text. A says `redraw=last`: bash redraws only a prompt's last line.
/// Needs bash 4.4 for `PS0`.
const BASH: &str = r#"# SonicTerm shell integration (bash 4.4+). Load with: eval "$(sonicterm-mcp --shell-integration bash)"
if [[ $- == *i* && -n "${SONICTERM_SHELL_TOKEN-}" && -z "${__sonicterm_token-}" ]] \
   && (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
  unset __sonicterm_token __sonicterm_b
  declare +x __sonicterm_token="$SONICTERM_SHELL_TOKEN"
  export -n __sonicterm_token
  unset SONICTERM_SHELL_TOKEN
  printf -v __sonicterm_b '\e]133;B;sonicterm_token=%s\a' "$__sonicterm_token"
  export -n __sonicterm_b
  __sonicterm_ran=0
  __sonicterm_hist=0
  __sonicterm_mark() { printf '\e]133;%s;sonicterm_token=%s\a' "$1" "$__sonicterm_token"; }
  __sonicterm_start() {
    local LC_ALL=C text= url= char index
    if (( ${HISTCMD:-0} > __sonicterm_hist )); then
      text=$(HISTTIMEFORMAT= history 1)
      text=${text#*[0-9][ *] }
    fi
    if (( ${#text} <= 1024 )); then
      for (( index = 0; index < ${#text}; index++ )); do
        char=${text:index:1}
        case $char in
          [A-Za-z0-9._~-]) url+=$char ;;
          *) printf -v char '%%%02X' "'$char"; url+=$char ;;
        esac
      done
    fi
    if [[ -n $url ]]; then __sonicterm_mark "C;cmdline_url=$url"; else __sonicterm_mark C; fi
  }
  __sonicterm_prompt() {
    local __sonicterm_status=$?
    if (( __sonicterm_ran )); then
      __sonicterm_mark "D;$__sonicterm_status;sonicterm_src=status"
    fi
    __sonicterm_ran=0
    __sonicterm_hist=${HISTCMD:-0}
    __sonicterm_mark "A;redraw=last"
    if [[ "$PS1" != *'${__sonicterm_b}'* ]]; then
      PS1="$PS1"'\[${__sonicterm_b}\]'
    fi
    return $__sonicterm_status
  }
  PROMPT_COMMAND="__sonicterm_prompt${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
  PS0='${__sonicterm_ran:$((__sonicterm_ran=1, 0)):0}$(__sonicterm_start)'"${PS0-}"
fi
"#;

/// PowerShell 7+: D and A from a wrapped `prompt`, B at the end of the prompt text, C from a
/// wrapped `PSConsoleHostReadLine` once PSReadLine returns a non-blank line, carrying that line
/// percent-encoded as `cmdline_url=` when its UTF-8 form is at most 1 KiB. PSReadLine keeps
/// editing incomplete input (an open `{`) inside its ReadLine, so a returned non-blank line is one
/// the shell will run. A blank or cancelled line gets no C, but the next prompt still writes D, so
/// a run that never started ends as `not_started`, as zsh's and bash's do. D's code: 0 when `$?`
/// held. Otherwise, a line whose commands are all cmdlets (read from its parsed syntax tree) can run
/// no native program, so it is 1. For any other line: a new error record and a non-zero
/// `$LASTEXITCODE` together give no code at all (a D with none), since which statement ended the
/// line is unknown even when the native code repeats; a new error record alone gives 1; a non-zero
/// `$LASTEXITCODE` alone gives that code (which a function failing silently may have left from an
/// earlier line); anything else gives 1. The snippet only reads
/// `$LASTEXITCODE`, so every command sees the value it would without integration. Without PSReadLine
/// the script loads nothing, so `run_command` refuses the pane instead of waiting on markers that never
/// come. The token lives only in a PowerShell variable, which no child process inherits. The
/// script is `scripts/shell-integration.ps1`, where first-party PowerShell lives.
const PWSH: &str = include_str!("../../../scripts/shell-integration.ps1");

#[cfg(all(test, unix))]
#[path = "shell_integration_tests.rs"]
mod shell_integration_tests;
