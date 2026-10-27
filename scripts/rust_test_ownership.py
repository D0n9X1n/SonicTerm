"""Validate flat Rust test-file ownership from actual cfg-gated module declarations."""

from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path, PurePosixPath
import posixpath
import re

from rust_comment_lexer import _delimiter_pairs, lex_rust


@dataclass(frozen=True)
class TestModule:
    """One external module declaration and the attributes attached to that item."""

    name: str
    path: str | None
    test_only: bool
    line: int


def _string_value(literal: str) -> str:
    """Decode the ordinary or raw Rust string accepted for a flat path attribute."""
    if literal.startswith('r'):
        match = re.fullmatch(r'r(#+)?"(.*)"\1', literal, re.DOTALL)
        if match:
            return match.group(2)
        if literal.startswith('r"') and literal.endswith('"'):
            return literal[2:-1]
        raise ValueError(f"unsupported path string: {literal}")
    value = json.loads(literal)
    if not isinstance(value, str):
        raise ValueError("a module path must be a string")
    return value


def _requires_test(tokens) -> bool:
    """Whether a cfg expression requires test; any(test, windows) does not."""
    words = [token.text for token in tokens]
    if words == ["test"]:
        return True
    if len(words) < 3 or words[0] not in ("all", "any") or words[1] != "(" or words[-1] != ")":
        return False
    groups = []
    start = 2
    depth = 0
    for index in range(2, len(words) - 1):
        if words[index] == "(":
            depth += 1
        elif words[index] == ")":
            depth -= 1
        elif words[index] == "," and depth == 0:
            if index > start:
                groups.append(tokens[start:index])
            start = index + 1
    if start < len(words) - 1:
        groups.append(tokens[start:-1])
    required = [_requires_test(group) for group in groups]
    return bool(required) and (any(required) if words[0] == "all" else all(required))


def declared_modules(source: str) -> list[TestModule]:
    """Read top-level declarations, ignoring comments, strings, macros and nested item bodies."""
    tokens = lex_rust(source.replace("\r\n", "\n")).tokens
    pairs = _delimiter_pairs(tokens)
    attributes = []
    modules = []
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token.text == "#" and index + 1 < len(tokens) and tokens[index + 1].text == "[":
            closing = pairs.get(index + 1)
            if closing is None:
                raise ValueError(f"unterminated attribute at line {token.line}")
            attributes.append(tokens[index + 2:closing])
            index = closing + 1
            continue
        cursor = index
        if token.text == "pub":
            cursor += 1
            if cursor < len(tokens) and tokens[cursor].text == "(":
                cursor = pairs.get(cursor, cursor) + 1
        if cursor + 2 < len(tokens) and tokens[cursor].text == "mod" and tokens[cursor + 1].kind == "IDENT":
            if tokens[cursor + 2].text == ";":
                paths = []
                test_only = False
                for attribute in attributes:
                    words = [part.text for part in attribute]
                    if len(attribute) == 3 and words[:2] == ["path", "="] and attribute[2].kind == "STRING":
                        paths.append(_string_value(words[2]))
                    if len(attribute) >= 3 and words[:2] == ["cfg", "("] and words[-1] == ")":
                        test_only |= _requires_test(attribute[2:-1])
                if len(paths) > 1:
                    raise ValueError(f"multiple path attributes at line {token.line}")
                modules.append(TestModule(tokens[cursor + 1].text.removeprefix("r#"),
                                          paths[0] if paths else None, test_only, token.line))
                attributes = []
                index = cursor + 3
                continue
        attributes = []
        if token.text in ("{", "(", "[") and index in pairs:
            index = pairs[index] + 1
        else:
            index += 1
    return modules


def _test_filename(name: str) -> bool:
    """Recognize suite names even when they violate the required owner/plural spelling."""
    return name in ("tests.rs", "test.rs") or name.endswith(("_tests.rs", "_test.rs"))


def _module_targets(owner: PurePosixPath, module: TestModule) -> list[str]:
    """Resolve explicit and default external paths without consulting the host's active cfgs."""
    if module.path is not None:
        return [posixpath.normpath((owner.parent / module.path).as_posix())]
    directory = owner.parent if owner.name in ("lib.rs", "main.rs", "mod.rs") else owner.parent / owner.stem
    return [(directory / (module.name + ".rs")).as_posix(), (directory / module.name / "mod.rs").as_posix()]


def _contains_test_attribute(source: str) -> bool:
    """Find real test/bench attributes, not source-scanner fixtures inside comments or literals."""
    tokens = lex_rust(source.replace("\r\n", "\n")).tokens
    pairs = _delimiter_pairs(tokens)
    index = 0
    while index < len(tokens):
        words = [token.text for token in tokens[index:index + 4]]
        # The lexer removes macro invocations; definitions have a name between ! and their body.
        if words[:2] == ["macro_rules", "!"] and len(words) == 4 and words[3] == "{" and index + 3 in pairs:
            index = pairs[index + 3] + 1
            continue
        if words in (["#", "[", "test", "]"], ["#", "[", "bench", "]"]):
            return True
        index += 1
    return False


def ownership_problems(sources: dict[str, str]) -> list[str]:
    """Require every tracked test file to have one sibling declaration from its named owner."""
    test_paths = {path for path in sources if _test_filename(PurePosixPath(path).name)}
    declarations = {path: [] for path in test_paths}
    problems = []
    for owner, source in sorted(sources.items()):
        owner_path = PurePosixPath(owner)
        try:
            modules = declared_modules(source)
        except (ValueError, json.JSONDecodeError) as error:
            problems.append(f"{owner}: cannot inspect module declarations: {error}")
            continue
        for module in modules:
            targets = _module_targets(owner_path, module)
            named_suite = (_test_filename(module.name + ".rs") or
                           any(_test_filename(PurePosixPath(target).name) for target in targets))
            # Test-gated build helpers and source scanners are not suites. Inspect their code so an
            # arbitrarily named file with real tests cannot use that distinction to escape naming.
            loaded_suite = module.test_only and any(
                target in sources and _contains_test_attribute(sources[target]) for target in targets)
            if not named_suite and not loaded_suite:
                continue
            prefix = f"{owner}:{module.line}"
            if module.path is None:
                problems.append(f"{prefix}: test module {module.name} requires an explicit #[path]")
                continue
            if not module.path.endswith("_tests.rs"):
                problems.append(f"{prefix}: test file {module.path} must be {owner_path.stem}_tests.rs "
                                f"or {owner_path.stem}_<topic>_tests.rs")
                continue
            if "/" in module.path or "\\" in module.path or PurePosixPath(module.path).name != module.path:
                problems.append(f"{prefix}: test path must name a flat sibling: {module.path}")
                continue
            target = owner_path.with_name(module.path).as_posix()
            if target not in test_paths:
                problems.append(f"{prefix}: declared test file is missing or untracked: {target}")
                continue
            declarations[target].append(owner)
            stem = owner_path.stem
            exact = stem + "_tests.rs"
            topic_prefix = stem + "_"
            topic = module.path[len(topic_prefix):-len("_tests.rs")] if module.path.startswith(topic_prefix) else ""
            if module.path != exact and not re.fullmatch(r"[a-z][a-z0-9]*(?:_[a-z0-9]+)*", topic):
                problems.append(f"{prefix}: {module.path} must be {exact} or {stem}_<topic>_tests.rs")
            candidates = [PurePosixPath(path) for path in sources
                          if PurePosixPath(path).parent == owner_path.parent and path != target]
            implied = [candidate for candidate in candidates
                       if module.path == candidate.stem + "_tests.rs"
                       or module.path.startswith(candidate.stem + "_")]
            if implied:
                closest = max(implied, key=lambda candidate: len(candidate.stem))
                if closest != owner_path:
                    problems.append(f"{prefix}: {module.path} names owner {closest.name}, not {owner_path.name}")
            if not module.test_only:
                problems.append(f"{prefix}: test declaration must require cfg(test): {module.path}")
    for path, owners in sorted(declarations.items()):
        if not owners:
            problems.append(f"{path}: no owning #[path] declaration")
        elif len(owners) != 1:
            problems.append(f"{path}: expected one owner declaration, found {len(owners)}: {', '.join(owners)}")
    return problems


def inspect_test_ownership(root: Path, paths: list[str], excluded_roots: tuple[str, ...]) -> list[str]:
    """Read tracked Rust, including generated roots that declare authored tests; never skip failed reads."""
    sources = {}
    problems = []
    for path in paths:
        if not path.endswith(".rs") or path.startswith(excluded_roots):
            continue
        try:
            sources[path] = (root / path).read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            problems.append(f"cannot read Rust source {path}: {error}")
    return problems + ownership_problems(sources)
