"""Rust comment test fixture."""

from __future__ import annotations
from contextlib import contextmanager
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap


_HERE = Path(__file__).resolve().parent
_CHECKER_PATH = _HERE / "check-authored-rust-comments.py"

_spec = importlib.util.spec_from_file_location("check_authored_rust_comments", _CHECKER_PATH)
checker = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = checker
_spec.loader.exec_module(checker)


def rust(source: str) -> str:
    """Return an LF-terminated, dedented Rust fixture."""
    return textwrap.dedent(source).lstrip("\n").rstrip() + "\n"


@contextmanager
def repository(files: dict[str, str | bytes]):
    """Create a tracked, uncommitted Git fixture repository."""
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        subprocess.run(
            ["git", "init", "-q", str(root)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        for name, content in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            if isinstance(content, bytes):
                path.write_bytes(content)
            else:
                with path.open("w", encoding="utf-8", newline="") as handle:
                    handle.write(content)
        subprocess.run(
            ["git", "-C", str(root), "add", "--", "."],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        yield root


def analyze(files: dict[str, str | bytes], paths: list[str] | None = None):
    """Analyze a temporary tracked fixture and return its report."""
    with repository(files) as root:
        return checker.analyze_repository(root, paths=paths, caller_cwd=root)


def formatted(report, rule: str | None = None) -> list[str]:
    """Format diagnostics, optionally selecting one rule."""
    return [
        diagnostic.format()
        for diagnostic in report.diagnostics
        if rule is None or diagnostic.rule == rule
    ]


def rule_names(report) -> list[str]:
    """Return diagnostic rule names in stable report order."""
    return [diagnostic.rule for diagnostic in report.diagnostics]


def cli(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run the checker CLI with platform newlines normalized for comparison."""
    return subprocess.run(
        [sys.executable, str(_CHECKER_PATH), *args],
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        text=True,
        encoding="utf-8",
    )
