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
}

impl Shell {
    /// The shell named `name`, as given on the command line.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            _ => None,
        }
    }

    /// The snippet for this shell.
    pub fn snippet(self) -> &'static str {
        match self {
            Self::Zsh => ZSH,
            Self::Bash => BASH,
        }
    }
}

/// zsh: D and A from `precmd` (D first, carrying the status the command left), B once the line
/// editor starts, C from `preexec`. Hooks are added beside any existing ones, never replacing them.
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
    if (( __sonicterm_ran )); then __sonicterm_mark "D;$__sonicterm_status"; fi
    __sonicterm_ran=0
    __sonicterm_mark A
    return $__sonicterm_status
  }
  __sonicterm_preexec() { __sonicterm_ran=1; __sonicterm_mark C; }
  __sonicterm_line_init() { __sonicterm_mark B; }
  autoload -Uz add-zsh-hook add-zle-hook-widget
  add-zsh-hook precmd __sonicterm_precmd
  add-zsh-hook preexec __sonicterm_preexec
  add-zle-hook-widget line-init __sonicterm_line_init
fi
"#;

/// bash: D and A from `PROMPT_COMMAND` (first, so it sees the command's status, which it
/// restores), B at the end of `PS1`, C from `PS0` as a command starts. Whether a command ran is
/// read from the prompt's command number (`\#`), which every executed command advances and an
/// empty line does not, whatever the history settings. The token and the B marker that carries
/// it are unexported (`export -n`), so even `set -a` cannot hand them to a command, and `PS1`
/// refers to the marker by name rather than holding it. Needs bash 4.4 for `PS0` and `@P`.
const BASH: &str = r#"# SonicTerm shell integration (bash 4.4+). Load with: eval "$(sonicterm-mcp --shell-integration bash)"
if [[ $- == *i* && -n "${SONICTERM_SHELL_TOKEN-}" && -z "${__sonicterm_token-}" ]] \
   && (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
  unset __sonicterm_token __sonicterm_b
  declare +x __sonicterm_token="$SONICTERM_SHELL_TOKEN"
  export -n __sonicterm_token
  unset SONICTERM_SHELL_TOKEN
  printf -v __sonicterm_b '\e]133;B;sonicterm_token=%s\a' "$__sonicterm_token"
  export -n __sonicterm_b
  __sonicterm_number='\#'
  __sonicterm_seen=""
  __sonicterm_mark() { printf '\e]133;%s;sonicterm_token=%s\a' "$1" "$__sonicterm_token"; }
  __sonicterm_prompt() {
    local __sonicterm_status=$? __sonicterm_now="${__sonicterm_number@P}"
    if [[ -n "$__sonicterm_seen" && "$__sonicterm_now" != "$__sonicterm_seen" ]]; then
      __sonicterm_mark "D;$__sonicterm_status"
    fi
    __sonicterm_seen=$__sonicterm_now
    __sonicterm_mark A
    if [[ "$PS1" != *'${__sonicterm_b}'* ]]; then
      PS1="$PS1"'\[${__sonicterm_b}\]'
    fi
    return $__sonicterm_status
  }
  PROMPT_COMMAND="__sonicterm_prompt${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
  PS0='$(__sonicterm_mark C)'"${PS0-}"
fi
"#;

#[cfg(all(test, unix))]
#[path = "shell_integration_tests.rs"]
mod shell_integration_tests;
