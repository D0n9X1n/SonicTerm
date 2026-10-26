#!/usr/bin/env python3
"""Enforce comments on authored Rust API and non-obvious runtime contracts.

The checker deliberately uses a small Rust-aware lexer instead of text grep.
It must distinguish source syntax from comments, attributes, macro token trees,
and shader/source strings before it can make useful claims about documentation.
"""

from __future__ import annotations
import argparse
from bisect import bisect_right
from dataclasses import dataclass, field
import json
from pathlib import Path
import posixpath
import re
import subprocess
import sys
from typing import Iterable, Iterator, Sequence

# Keep the existing script import surface; implementation modules own their state.
from rust_comment_model import (
    EXCLUDED_PREFIXES,
    AUTHORED_VENDOR_FILES,
    EXCLUDED_FILES,
    MARKERS,
    STOPWORDS,
    ATOMIC_ORDERINGS,
    IDENTIFIER_STOPLIST,
    CONTRACT_MARKERS,
    RUST_KEYWORDS,
    MAX_MARKER_LINES,
    MAX_MARKER_CHARACTERS,
    OPEN_TO_CLOSE,
    CLOSE_TO_OPEN,
    Token,
    Comment,
    Lexed,
    Diagnostic,
    Report,
    Span,
    MarkerInstance,
    Branch,
    Function,
    Module,
    SourceUnit,
    AnalysisError,
)
from rust_comment_lexer import (
    _IDENTIFIER_START,
    _IDENTIFIER_CONTINUE,
    _line_starts,
    _location,
    _raw_string_end,
    _quoted_end,
    _char_or_lifetime,
    lex_rust,
    _delimiter_pairs,
    _attributes,
)
from rust_comment_sources import (
    _is_excluded,
    _is_test_file,
    _is_binary,
    _normalize_path,
    _tracked_rust,
    _source_unit,
    _span_ending,
    _prelude,
    _attribute_texts,
    _doc_text,
)
from rust_comment_markers import (
    _marker_name,
    _marker_body,
    _substantive,
    _exact_identifier_tokens,
    _ordinary_line_comment,
    _contiguous_line_comments_before,
    _marker_instances,
    _strict_prelude_instances,
    _first_body_instances,
    _marker_size_error,
    _semantic_tokens,
    _comments_between,
    _first_nonspace_offset,
)
from rust_comment_items import (
    _matching,
    _token_index_at_or_after,
    _find_body,
    _signature_start,
    _reverse_match,
    _lexical_visibility,
    _top_level_indices,
)
from rust_comment_modules import (
    _module_target,
    _preceding_attributes,
    _path_attribute,
    _cfg_test,
    _direct_test_item,
    _pub_use_names,
    _build_modules,
)
from rust_comment_functions import (
    _find_traits_and_impls,
    _functions,
    _non_safety_exempt_at,
    _enclosing,
    _function_effective_visibility,
)
from rust_comment_contracts import (
    _comment_markers,
    _use_instances,
    _instance_tokens,
    _validate_instance_sizes,
    _attach_marker,
    _rule_for_marker,
    _nested_function_ranges,
    _function_body_tokens,
    _inside_ranges,
    _outside_ranges,
    _receiver_identifier,
    _lock_identifiers,
    COUNTED_LOCK_HELPERS,
    _counted_lock_identifiers,
    _atomic_sites,
    _release_tokens,
    _lock_sequence_text,
    _lock_validator,
    _ordering_validator,
    _lifecycle_validator,
)
from rust_comment_branches import (
    _selecting_tokens,
    _predicate_tokens,
    _validate_when_text,
    _first_body_comments,
    _contains,
    _ignored_result,
    _platform_tokens,
    _shared_when_instances,
    _branch_marker,
)
from rust_comment_control_flow import (
    _permitted_path,
    _permitted_index_key,
    _parse_permitted_leaf,
    _permitted_leaf,
    _if_has_expression_parent,
    _top_level_token,
    _brace_starts_condition_expression,
    _if_expression_end,
    _if_body_open,
    _match_body_open,
    _if_branches,
    _statement_let,
    _let_else_branches,
    _match_arms,
)
from rust_comment_analysis import (
    _analyze_unit,
    _crate_key,
    analyze_repository,
)
from rust_comment_cli import (
    _repository_root,
    _parser,
    main,
)

if __name__ == "__main__":
    raise SystemExit(main())
