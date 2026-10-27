"""Gate commands."""

from __future__ import annotations
from dataclasses import dataclass, replace
import re
from typing import Iterable, Mapping, Sequence, TextIO

import gate_ci_catalog


@dataclass(frozen=True)
class Invocation:
    """One gate command after spelling normalization: a cargo gate subcommand or a first-party script."""

    # "cargo" or "script".
    kind: str
    # The cargo subcommand with aliases resolved, or the script's file name under scripts/.
    name: str
    args: tuple[str, ...]
    env: tuple[str, ...] = ()
    toolchain: str | None = None


class _Unsupported(ValueError):
    """A shell spelling the gate classifier does not model."""


_WORD_END = frozenset(" \t;&|()<>")
# Unquoted, bash reads a backslash before these as an escape and PowerShell as a
# literal, so the line means different things to the two shells.
_AMBIGUOUS_ESCAPE = frozenset(" \t\"'$`#;&|()<>\\")


def _substitution_span(line: str, index: int) -> int:
    """Return the index just past a `$(...)` or backtick substitution that starts at index."""
    if line[index] == "`":
        end = line.find("`", index + 1)
        if end < 0:
            raise _Unsupported("an unterminated backtick")
        return end + 1
    depth = 1
    quote = ""
    index += 2
    while index < len(line):
        char = line[index]
        if quote:
            if char == "\\" and quote == '"':
                index += 1
            elif char == quote:
                quote = ""
        elif char in "'\"":
            quote = char
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return index + 1
        index += 1
    raise _Unsupported("an unterminated command substitution")


def _read_word(line: str, index: int) -> tuple[str, bool, bool, int]:
    """Read one shell word from index.

    Returns the word with its quotes removed, whether any part was quoted, whether it
    holds a command substitution, and the index just past it.
    """
    parts: list[str] = []
    quoted = substitutes = False
    while index < len(line) and line[index] not in _WORD_END:
        char = line[index]
        if char == "'":
            end = line.find("'", index + 1)
            if end < 0:
                raise _Unsupported("an unterminated single quote")
            parts.append(line[index + 1:end])
            quoted, index = True, end + 1
        elif char == '"':
            quoted, index = True, index + 1
            while True:
                if index >= len(line):
                    raise _Unsupported("an unterminated double quote")
                char = line[index]
                if char == '"':
                    index += 1
                    break
                if char == "\\" and line[index + 1:index + 2] in ('"', "\\", "$", "`"):
                    parts.append(line[index + 1])
                    index += 2
                elif char == "`" or line.startswith("$(", index):
                    end = _substitution_span(line, index)
                    parts.append(line[index:end])
                    substitutes, index = True, end
                else:
                    parts.append(char)
                    index += 1
        elif char == "`" or line.startswith("$(", index):
            end = _substitution_span(line, index)
            parts.append(line[index:end])
            substitutes, index = True, end
        elif char == "\\" and (index + 1 >= len(line) or line[index + 1] in _AMBIGUOUS_ESCAPE):
            raise _Unsupported("a backslash escape, which bash and PowerShell read differently")
        else:
            parts.append(char)
            index += 1
    return "".join(parts), quoted, substitutes, index


def _tokens(line: str) -> list[tuple[str, str, bool]]:
    """Split one logical shell line into ("word", text, substitutes) and ("op", text, False) tokens."""
    tokens: list[tuple[str, str, bool]] = []
    index = 0
    while index < len(line):
        char = line[index]
        if char in " \t":
            index += 1
        elif char == "#":
            # A word that starts with `#` begins a comment in bash and PowerShell.
            break
        elif char in "<>" or line.startswith("&>", index):
            index += 2 if char == "&" else 1
            if line[index:index + 1] in ("<", ">", "&"):
                index += 1
            tokens.append(("op", "redirect", False))
        elif char in ";&|()":
            pair = line[index:index + 2]
            text = pair if pair in ("&&", "||", "|&", ";;") else char
            index += len(text)
            tokens.append(("op", text, False))
        else:
            word, quoted, substitutes, end = _read_word(line, index)
            index = end
            if not quoted and word.isdigit() and line[end:end + 1] in ("<", ">"):
                # A file-descriptor number belongs to the redirect after it.
                continue
            tokens.append(("word", word, substitutes))
    return tokens


def _simple_commands(
    tokens: Sequence[tuple[str, str, bool]]
) -> list[tuple[tuple[tuple[str, bool], ...], str, str, bool]]:
    """Group tokens into (words, operator before, operator after, redirected) simple commands."""
    commands = []
    words: list[tuple[str, bool]] = []
    before = ""
    redirected = False
    for kind, text, substitutes in tokens:
        if kind == "word":
            words.append((text, substitutes))
        elif text == "redirect":
            redirected = True
        else:
            commands.append((tuple(words), before, text, redirected))
            words, before, redirected = [], text, False
    commands.append((tuple(words), before, "", redirected))
    return [command for command in commands if command[0]]


def _script_name(word: str) -> str | None:
    """Return the file name of a first-party scripts/ path in either separator style, or None."""
    match = gate_ci_catalog._SCRIPT_PATH.fullmatch(word.replace("\\", "/"))
    return match.group(1) if match else None


def _program_name(word: str) -> str:
    """Return a command word's lowercase basename without a Windows `.exe` suffix."""
    name = re.split(r"[/\\]", word)[-1].lower()
    return name[:-4] if name.endswith(".exe") else name


def _gate_word(word: str) -> bool:
    """Report whether a word is cargo itself or names a first-party script path."""
    return _program_name(word) == "cargo" or bool(gate_ci_catalog._SCRIPT_MENTION.search(word))


def _classify_cargo(
    args: tuple[str, ...], env: tuple[str, ...]
) -> tuple[list[Invocation], list[str]]:
    """Classify a cargo command, normalizing a `+toolchain` override and the gate aliases."""
    toolchain = None
    if args and args[0].startswith("+"):
        toolchain, args = args[0], args[1:]
    if not args:
        return [], []
    if gate_ci_catalog._EXPRESSION_WORD in args[0]:
        return [], ["a cargo subcommand that a workflow expression supplies"]
    subcommand = gate_ci_catalog._CARGO_ALIASES.get(args[0], args[0])
    if subcommand in gate_ci_catalog._CARGO_GATES:
        return [Invocation("cargo", subcommand, args[1:], env, toolchain)], []
    if args[0].startswith("-"):
        # After a leading option, any later gate word or expression may be the subcommand.
        if any(gate_ci_catalog._CARGO_ALIASES.get(word, word) in gate_ci_catalog._CARGO_GATES for word in args):
            return [], ["a cargo option before the gate subcommand"]
        if any(gate_ci_catalog._EXPRESSION_WORD in word for word in args):
            return [], ["a cargo option before a subcommand that a workflow expression may supply"]
    return [], []


def _classify_script(
    name: str, args: tuple[str, ...], env: tuple[str, ...]
) -> tuple[list[Invocation], list[str]]:
    """Classify a first-party script and the command it runs after a `--` separator, if any."""
    head, nested = args, ()
    if "--" in args:
        split = args.index("--")
        head, nested = args[:split], args[split + 1:]
    invocations = [Invocation("script", name, args, env)]
    reasons = []
    if any(_gate_word(word) for word in head):
        reasons.append(f"cargo or a script path as an argument to scripts/{name}")
    if nested:
        found, why = _classify_words(tuple((word, False) for word in nested))
        invocations.extend(found)
        reasons.extend(why)
    return invocations, reasons


def _classify_words(words: Sequence[tuple[str, bool]]) -> tuple[list[Invocation], list[str]]:
    """Classify one simple command's words, after its leading variable assignments."""
    reasons = [
        "a gate inside a command substitution"
        for text, substitutes in words if substitutes and gate_ci_catalog._GATE_MENTION.search(text)
    ]
    index = 0
    while index < len(words) and gate_ci_catalog._ASSIGNMENT.fullmatch(words[index][0]):
        index += 1
    env = tuple(text for text, _substitutes in words[:index])
    rest = [text for text, _substitutes in words[index:]]
    if not rest:
        return [], reasons
    program, args = rest[0], tuple(rest[1:])
    if gate_ci_catalog._EXPRESSION_WORD in program:
        return [], reasons + ["a command word that a workflow expression supplies"]
    name = _program_name(program)
    if name == "cargo":
        found, why = _classify_cargo(args, env)
        return found, reasons + why
    script = _script_name(program)
    if script is None and args and (name in gate_ci_catalog._INTERPRETERS or gate_ci_catalog._VARIABLE_WORD.fullmatch(program)):
        # An interpreter, or a variable holding one, runs the script path it is given first.
        if gate_ci_catalog._EXPRESSION_WORD in args[0] or (
            args[0].startswith("-") and any(gate_ci_catalog._EXPRESSION_WORD in word for word in args)
        ):
            # The expression may be the script itself, or follow options that make it the script.
            return [], reasons + [f"a script argument to `{program}` that a workflow expression may supply"]
        script = _script_name(args[0])
        args = args[1:] if script is not None else args
    if script is not None:
        found, why = _classify_script(script, args, env)
        return found, reasons + why
    if any(_gate_word(word) for word in rest) or gate_ci_catalog._GATE_MENTION.search(" ".join(rest)):
        reasons.append(f"a gate after `{program}`, which the classifier does not model as a command")
    return [], reasons


def classify_command(line: str) -> tuple[list[Invocation], list[str]]:
    """Normalize one logical run line into its gate invocations and the spellings it cannot model.

    Every simple command joined by `&&` or `;` is classified. A gate the classifier cannot
    prove runs with its exit status observed, such as one behind a pipe, `||`, a wrapper
    command, or a command substitution, yields a reason and is never skipped.
    """
    try:
        tokens = _tokens(gate_ci_catalog._EXPRESSION.sub(gate_ci_catalog._EXPRESSION_WORD, line))
    except _Unsupported as error:
        return [], [str(error)] if gate_ci_catalog._GATE_MENTION.search(line) else []
    invocations: list[Invocation] = []
    reasons: list[str] = []
    for words, before, after, redirected in _simple_commands(tokens):
        found, why = _classify_words(words)
        invocations.extend(found)
        reasons.extend(why)
        if not (found or why):
            continue
        operators = [operator for operator in (before, after) if operator in gate_ci_catalog._UNMODELED_OPERATORS]
        if operators:
            reasons.append(f"a gate beside `{operators[0]}`, which can hide its exit status")
        if redirected:
            reasons.append("a redirected gate")
    return invocations, reasons


def is_self_test(invocation: Invocation) -> bool:
    """Report whether an invocation is a first-party test entry point, which must be a table step."""
    if invocation.kind != "script":
        return False
    return bool(
        gate_ci_catalog._SELF_TEST_NAME.fullmatch(invocation.name)
        or (gate_ci_catalog._BARE_TEST_NAME.fullmatch(invocation.name) and not invocation.args)
    )


_KEY = re.compile(r"([A-Za-z0-9_-]+):(?:[ \t]+(.*))?")
_JOB_ID = re.compile(r"[A-Za-z_][A-Za-z0-9_-]*")
_JOBS_KEY = re.compile(r"jobs:[ \t]*(?:#.*)?")
_BLOCK_HEADER = re.compile(r"\|[-+]?(?:[ \t]+#.*)?")
