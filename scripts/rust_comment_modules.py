"""Rust comment modules."""

from __future__ import annotations
from pathlib import Path
import posixpath
import re

import rust_comment_model


def _module_target(parent: rust_comment_model.Module, name: str, path_attr: str | None, tracked: set[str]) -> str | None:
    parent_path = Path(parent.unit.path)
    if path_attr:
        candidate = posixpath.normpath((parent_path.parent / path_attr).as_posix())
        return candidate if candidate in tracked else None
    if parent_path.name in {"lib.rs", "main.rs", "mod.rs"}:
        base = parent_path.parent
    else:
        base = parent_path.parent / parent_path.stem
    name = name.removeprefix("r#")
    candidates = [(base / f"{name}.rs").as_posix(), (base / name / "mod.rs").as_posix()]
    return next((candidate for candidate in candidates if candidate in tracked), None)


def _preceding_attributes(unit: rust_comment_model.SourceUnit, token: rust_comment_model.Token) -> tuple[list[rust_comment_model.Comment], list[rust_comment_model.Span]]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_sources
    comments, attributes, _ = rust_comment_sources._prelude(unit, token.start)
    return comments, attributes


def _path_attribute(unit: rust_comment_model.SourceUnit, token: rust_comment_model.Token) -> str | None:
    _, attributes = _preceding_attributes(unit, token)
    for span in attributes:
        text = unit.text[span.start:span.end]
        match = re.search(r"\bpath\s*=\s*\"([^\"]+)\"", text)
        if match:
            return match.group(1)
    return None


def _cfg_test(unit: rust_comment_model.SourceUnit, token: rust_comment_model.Token) -> bool:
    _, attributes = _preceding_attributes(unit, token)
    return any(re.search(r"\bcfg\s*\(\s*test\s*\)", unit.text[span.start:span.end]) for span in attributes)


def _direct_test_item(unit: rust_comment_model.SourceUnit, token: rust_comment_model.Token) -> bool:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_sources
    _, attributes = _preceding_attributes(unit, token)
    texts = rust_comment_sources._attribute_texts(unit, attributes)
    return any(re.search(r"#\[\s*(?:test|bench)\b", text) for text in texts)


def _pub_use_names(unit: rust_comment_model.SourceUnit, module: rust_comment_model.Module, start_index: int, end_index: int) -> set[str]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    names: set[str] = set()
    index = start_index
    while index < end_index - 1:
        if unit.tokens[index].text == "pub" and unit.tokens[index + 1].text == "use":
            cursor = index + 2
            parts: list[str] = []
            while cursor < end_index and unit.tokens[cursor].text != ";":
                if unit.tokens[cursor].kind == "IDENT":
                    parts.append(unit.tokens[cursor].text)
                cursor += 1
            while parts and parts[0] in {"crate", "self", "super"}:
                parts.pop(0)
            if parts:
                names.add(parts[0])
            index = cursor + 1
            continue
        if unit.tokens[index].text in rust_comment_model.OPEN_TO_CLOSE and index in unit.pairs:
            index = unit.pairs[index] + 1
        else:
            index += 1
    return names


def _build_modules(units: dict[str, rust_comment_model.SourceUnit], tracked: set[str], diagnostics: list[rust_comment_model.Diagnostic]) -> list[rust_comment_model.Module]:
    # Resolve this runtime-only dependency after module initialization.
    import rust_comment_model
    modules: list[rust_comment_model.Module] = []
    visited_files: set[tuple[str, tuple[str, ...]]] = set()

    def add_module(module: rust_comment_model.Module, start_index: int, end_index: int) -> None:
        # Resolve this runtime-only dependency after module initialization.
        import rust_comment_model
        key = (module.unit.path, module.key)
        if key in visited_files:
            return
        visited_files.add(key)
        module.unit.modules.append(module)
        modules.append(module)
        module.reexports = _pub_use_names(module.unit, module, start_index, end_index)
        index = start_index
        while index < end_index:
            token = module.unit.tokens[index]
            public = False
            mod_index = index
            if token.text == "pub" and index + 1 < end_index and module.unit.tokens[index + 1].text == "mod":
                public = True
                mod_index = index + 1
            elif token.text != "mod":
                if token.text in rust_comment_model.OPEN_TO_CLOSE and index in module.unit.pairs:
                    index = module.unit.pairs[index] + 1
                else:
                    index += 1
                continue
            if mod_index + 1 >= end_index or module.unit.tokens[mod_index + 1].kind != "IDENT":
                index += 1
                continue
            name_token = module.unit.tokens[mod_index + 1]
            after = mod_index + 2
            if after >= end_index:
                break
            test_context = module.test_context or _cfg_test(module.unit, token)
            if module.unit.tokens[after].text == "{":
                close = module.unit.pairs.get(after)
                if close is None:
                    index += 1
                    continue
                child = rust_comment_model.Module(
                    key=module.key + (name_token.text,),
                    unit=module.unit,
                    start=module.unit.tokens[after].end,
                    end=module.unit.tokens[close].start,
                    parent=module,
                    name=name_token.text,
                    public_decl=public,
                    test_context=test_context,
                )
                module.children.append(child)
                add_module(child, after + 1, close)
                index = close + 1
                continue
            if module.unit.tokens[after].text == ";":
                target = _module_target(module, name_token.text, _path_attribute(module.unit, token), tracked)
                if target is None:
                    diagnostics.append(
                        rust_comment_model.Diagnostic(
                            module.unit.path,
                            token.line,
                            token.column,
                            "module-resolution",
                            f"module {name_token.text} has no tracked source target",
                        )
                    )
                    index = after + 1
                    continue
                target_unit = units[target]
                child = rust_comment_model.Module(
                    key=module.key + (name_token.text,),
                    unit=target_unit,
                    start=0,
                    end=len(target_unit.text),
                    parent=module,
                    name=name_token.text,
                    public_decl=public,
                    test_context=test_context or target_unit.file_test_context,
                )
                module.children.append(child)
                add_module(child, 0, len(target_unit.tokens))
                index = after + 1
                continue
            index += 1

    roots = [path for path in units if path == "src/lib.rs" or path.endswith("/src/lib.rs")]
    for path in sorted(roots):
        unit = units[path]
        crate = path[: -len("/src/lib.rs")] if path.endswith("/src/lib.rs") else "crate"
        root = rust_comment_model.Module((crate,), unit, 0, len(unit.text), None, crate, True, False, reachable=True)
        add_module(root, 0, len(unit.tokens))

    changed = True
    while changed:
        changed = False
        for module in modules:
            if module.parent is None:
                continue
            should_reach = module.parent.reachable and (
                module.public_decl or module.name in module.parent.reexports
            )
            if should_reach and not module.reachable:
                module.reachable = True
                changed = True
    return modules
