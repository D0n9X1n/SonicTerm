"""Rust comment model."""

from __future__ import annotations
from bisect import bisect_right
from dataclasses import dataclass, field



EXCLUDED_PREFIXES = (
    "crates/sonicterm-freetype/freetype2/",
    "crates/sonicterm-freetype/libpng/",
    "crates/sonicterm-freetype/zlib/",
    "crates/sonicterm-harfbuzz/harfbuzz/",
    "crates/sonicterm-winit/",
)
AUTHORED_VENDOR_FILES = frozenset(
    {"crates/sonicterm-winit/src/platform_impl/windows/keyboard_tests.rs"}
)
EXCLUDED_FILES = frozenset(
    {
        "crates/sonicterm-freetype/src/lib.rs",
        "crates/sonicterm-freetype/src/types.rs",
        "crates/sonicterm-harfbuzz/src/lib.rs",
        "crates/sonicterm-fontconfig/src/lib.rs",
        "crates/sonicterm-block-glyph/src/customglyph.rs",
    }
)
MARKERS = ("When", "SAFETY", "Lock order", "Ordering", "Lifecycle")
STOPWORDS = frozenset(
    {
        "a",
        "an",
        "and",
        "are",
        "as",
        "at",
        "be",
        "because",
        "been",
        "being",
        "by",
        "condition",
        "else",
        "false",
        "for",
        "from",
        "has",
        "have",
        "if",
        "in",
        "into",
        "is",
        "it",
        "its",
        "not",
        "of",
        "on",
        "or",
        "path",
        "predicate",
        "branch",
        "that",
        "the",
        "then",
        "this",
        "to",
        "true",
        "was",
        "when",
        "where",
        "which",
        "with",
    }
)
ATOMIC_ORDERINGS = frozenset({"Relaxed", "Acquire", "Release", "AcqRel", "SeqCst"})
IDENTIFIER_STOPLIST = frozenset(
    {"self", "Some", "None", "Ok", "Err", "true", "false", "if", "else", "match", "let", "return", "_"}
)
CONTRACT_MARKERS = frozenset({"When", "Lock order", "Ordering", "Lifecycle"})
RUST_KEYWORDS = frozenset(
    {
        "Self",
        "abstract",
        "as",
        "async",
        "await",
        "become",
        "box",
        "break",
        "const",
        "continue",
        "crate",
        "do",
        "dyn",
        "else",
        "enum",
        "extern",
        "false",
        "final",
        "fn",
        "for",
        "gen",
        "if",
        "impl",
        "in",
        "let",
        "loop",
        "macro",
        "match",
        "mod",
        "move",
        "mut",
        "override",
        "priv",
        "pub",
        "ref",
        "return",
        "self",
        "static",
        "struct",
        "super",
        "trait",
        "true",
        "try",
        "type",
        "typeof",
        "union",
        "unsafe",
        "unsized",
        "use",
        "virtual",
        "where",
        "while",
        "yield",
    }
)
MAX_MARKER_LINES = 2
MAX_MARKER_CHARACTERS = 160
OPEN_TO_CLOSE = {"(": ")", "[": "]", "{": "}"}
CLOSE_TO_OPEN = {value: key for key, value in OPEN_TO_CLOSE.items()}


@dataclass(frozen=True)
class Token:
    """One non-comment Rust token with a source location."""

    kind: str
    text: str
    start: int
    end: int
    line: int
    column: int


@dataclass(frozen=True)
class Comment:
    """One line or nested block comment preserved for attachment checks."""

    text: str
    start: int
    end: int
    line: int
    column: int


@dataclass
class Lexed:
    """Tokenized Rust source plus comments and line-offset lookup data."""

    source: str
    tokens: list[Token]
    comments: list[Comment]
    line_starts: list[int]

    def line_column(self, offset: int) -> tuple[int, int]:
        """Return a one-based Unicode-codepoint line and column."""
        line_index = bisect_right(self.line_starts, offset) - 1
        return line_index + 1, offset - self.line_starts[line_index] + 1


@dataclass(frozen=True, order=True)
class Diagnostic:
    """A stable source diagnostic emitted by one checker rule."""

    path: str
    line: int
    column: int
    rule: str
    message: str

    def format(self) -> str:
        """Render the machine-stable diagnostic format used by CI."""
        return f"{self.path}:{self.line}:{self.column} [{self.rule}] {self.message}"


@dataclass
class Report:
    """Repository analysis results and the inventory behind them."""

    diagnostics: list[Diagnostic]
    semantic_candidates: list[Diagnostic]
    counts: dict
    paths: dict[str, list[str]]

    def inventory(self) -> dict:
        """Return a deterministic JSON-compatible inventory."""
        return {
            "counts": self.counts,
            "paths": self.paths,
            "diagnostics": [item.format() for item in self.diagnostics],
            "semantic_candidates": [item.format() for item in self.semantic_candidates],
        }


@dataclass
class Span:
    """A source range used for comments, attributes, and syntax bodies."""

    start: int
    end: int


@dataclass(frozen=True)
class MarkerInstance:
    """One marker line and its contiguous continuation comments."""

    marker: str
    comments: tuple[Comment, ...]

    @property
    def anchor(self) -> Comment:
        """Return the marker's prefixed first line."""
        return self.comments[0]

    @property
    def body(self) -> str:
        """Return marker prose without comment prefixes or the class label."""
        # Resolve this runtime-only dependency after module initialization.
        import rust_comment_markers
        pieces = [rust_comment_markers._marker_body(self.comments[0], self.marker)]
        pieces.extend(comment.text.strip()[2:].strip() for comment in self.comments[1:])
        return " ".join(piece for piece in pieces if piece)


@dataclass
class Branch:
    """One conditional arm and the construct that owns its contract scope."""

    anchor: Token
    body_start: int
    body_end: int
    selecting: set[str]
    predicate: set[str]
    mandatory: bool
    value_selector: bool
    construct: Token | None


@dataclass
class Function:
    """A parsed named function or method and its enclosing context."""

    unit: "SourceUnit"
    token_index: int
    start_index: int
    name: str
    body_open: int | None
    body_close: int | None
    lexical_public: bool
    restricted_public: bool
    unsafe: bool
    nested: bool = False
    test_context: bool = False
    effective_public: bool = False
    public_trait_member: bool = False

    @property
    def start_token(self) -> Token:
        """Return the first signature modifier used as diagnostic anchor."""
        return self.unit.tokens[self.start_index]


@dataclass
class Module:
    """One file-backed or inline Rust module in the resolution graph."""

    key: tuple[str, ...]
    unit: "SourceUnit"
    start: int
    end: int
    parent: "Module | None"
    name: str
    public_decl: bool
    test_context: bool
    reachable: bool = False
    reexports: set[str] = field(default_factory=set)
    children: list["Module"] = field(default_factory=list)


@dataclass
class SourceUnit:
    """A tracked Rust file with token and delimiter indexes."""

    path: str
    text: str
    lexed: Lexed
    tokens: list[Token]
    pairs: dict[int, int]
    reverse_pairs: dict[int, int]
    attributes: list[Span]
    file_test_context: bool
    binary_context: bool
    excluded: bool
    modules: list[Module] = field(default_factory=list)

    def deepest_module(self, offset: int) -> Module | None:
        """Return the smallest module range containing an offset."""
        matches = [module for module in self.modules if module.start <= offset < module.end]
        return min(matches, key=lambda module: module.end - module.start, default=None)


class AnalysisError(RuntimeError):
    """Raised when repository discovery cannot produce a meaningful corpus."""
