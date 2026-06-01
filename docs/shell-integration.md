# Shell integration (OSC 133)

SonicTerm understands the [FinalTerm / WezTerm OSC 133 prompt-marking
protocol](https://wezfurlong.org/wezterm/shell-integration.html). When your
shell emits these markers, SonicTerm records each prompt's start row, end row,
and exit code in the grid and:

- Draws a small caret in the left gutter at every visible prompt row.
- Lets you jump between prompts with `super+up` / `super+down`
  (`ScrollToPrevPrompt` / `ScrollToNextPrompt`).

Markers used:

| Sequence              | Meaning                                  |
| --------------------- | ---------------------------------------- |
| `OSC 133 ; A ST`      | Prompt start                             |
| `OSC 133 ; B ST`      | Prompt end / command-line edit start     |
| `OSC 133 ; C ST`      | Command output start                     |
| `OSC 133 ; D ; <n> ST`| Command finished, exit code `n`          |

`ST` is the string terminator (`ESC \` or `BEL` / `0x07`). `OSC` is
`ESC ]` (`0x1b 0x5d`).

## Opt-in snippets

These are minimal; full WezTerm snippets work too.

### zsh — `~/.zshrc`

```zsh
precmd()  { print -Pn "\e]133;A\a" }
preexec() { print -Pn "\e]133;C\a" }

# Capture exit status of the just-finished command.
PROMPT="%{$(print -Pn '\e]133;D;%?\a')%}$PROMPT"
PROMPT="%{$(print -Pn '\e]133;A\a')%}$PROMPT"
```

### bash — `~/.bashrc`

```bash
__sonic_prompt() { printf '\e]133;D;%s\a\e]133;A\a' "$?"; }
PROMPT_COMMAND="__sonic_prompt${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
trap 'printf "\e]133;C\a"' DEBUG
```

### fish — `~/.config/fish/conf.d/sonic.fish`

```fish
function __sonic_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end

function __sonic_postexec --on-event fish_postexec
    printf '\e]133;D;%s\a' $status
end

function fish_prompt --description 'Write OSC 133 A then the real prompt'
    printf '\e]133;A\a'
    # ... your existing prompt body here ...
end
```

## Notes

- SonicTerm keeps the **last 256** prompt regions per pane. Older ones are
  discarded silently.
- A repeated `A` on the same row is coalesced — emitting the marker more
  than once per prompt is safe.
- A stray `D` with no preceding `A` is ignored.
