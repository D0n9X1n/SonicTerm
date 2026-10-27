"""Perf expression fixture."""

from __future__ import annotations
import re



def job_step(workflow, job_id, name):
    """The one step of a job with this name."""
    found = [step for step in workflow["jobs"][job_id]["steps"] if step.get("name") == name]
    if len(found) != 1:
        raise AssertionError(f"{job_id} has {len(found)} steps named {name!r}")
    return found[0]


class UnsupportedExpression(ValueError):
    """An expression form the simulator does not model; a test never guesses at its meaning."""


_EXPRESSION_TOKEN = re.compile(r"\s*(?:(?P<string>'(?:[^']|'')*')|(?P<operator>==|!=|&&|\|\||[!(),])"
                               r"|(?P<name>[A-Za-z_][A-Za-z0-9_.*-]*))")
STATUS_FUNCTIONS = ("always", "success", "failure", "cancelled")
# What an `if:` may read (eligibility reads the pull request's action and labels); a `${{ }}` value may
# also read the rest of github, needs, matrix and runner.temp.
IF_REFERENCES = (re.compile(r"steps\.[A-Za-z0-9_-]+\.outputs\.[A-Za-z0-9_-]+"), re.compile(r"runner\.os"),
                 re.compile(r"github\.event_name"), re.compile(r"github\.event\.action"),
                 re.compile(r"github\.event\.label\.name"),
                 re.compile(r"github\.event\.pull_request\.labels\.\*\.name"))
VALUE_REFERENCES = IF_REFERENCES + (
    re.compile(r"github\.[A-Za-z0-9_.]+"), re.compile(r"matrix\.[A-Za-z0-9_]+"), re.compile(r"runner\.temp"),
    re.compile(r"needs\.[A-Za-z0-9_-]+\.(?:result|outputs\.[A-Za-z0-9_-]+)"))


class _ExpressionParser:
    """Recursive descent over `||`, `&&`, `!`, `==`, `!=`, parentheses, string literals, calls and references."""

    def __init__(self, text):
        self.tokens = []
        position, text = 0, text.strip()
        while position < len(text):
            match = _EXPRESSION_TOKEN.match(text, position)
            if match is None or match.end() == position:
                raise UnsupportedExpression(f"cannot read {text[position:]!r}")
            self.tokens.append((match.lastgroup, match.group(match.lastgroup)))
            position = match.end()
        self.position = 0

    def peek(self):
        return self.tokens[self.position] if self.position < len(self.tokens) else (None, None)

    def take(self, expected=None):
        token = self.peek()
        if token[0] is None or (expected is not None and token != ("operator", expected)):
            raise UnsupportedExpression(f"expected {expected or 'a token'}, found {token[1]!r}")
        self.position += 1
        return token

    def parse(self):
        tree = self.disjunction()
        if self.position != len(self.tokens):
            raise UnsupportedExpression(f"trailing {self.tokens[self.position:]}")
        return tree

    def disjunction(self):
        tree = self.conjunction()
        while self.peek() == ("operator", "||"):
            self.take()
            tree = ("or", tree, self.conjunction())
        return tree

    def conjunction(self):
        tree = self.unary()
        while self.peek() == ("operator", "&&"):
            self.take()
            tree = ("and", tree, self.unary())
        return tree

    def unary(self):
        if self.peek() == ("operator", "!"):
            self.take()
            return ("not", self.unary())
        tree = self.primary()
        if self.peek() in (("operator", "=="), ("operator", "!=")):
            operator = self.take()[1]
            tree = ("eq" if operator == "==" else "ne", tree, self.primary())
        return tree

    def primary(self):
        kind, value = self.take()
        if (kind, value) == ("operator", "("):
            tree = self.disjunction()
            self.take(")")
            return tree
        if kind == "string":
            return ("literal", value[1:-1].replace("''", "'"))
        if kind != "name":
            raise UnsupportedExpression(f"unexpected {value!r}")
        if self.peek() != ("operator", "("):
            return ("reference", value)
        self.take("(")
        arguments = []
        if self.peek() != ("operator", ")"):
            arguments.append(self.disjunction())
            while self.peek() == ("operator", ","):
                self.take()
                arguments.append(self.disjunction())
        self.take(")")
        return ("call", value, arguments)


def _truthy(value):
    """GitHub's coercion for the values modelled here: a non-empty string or True."""
    return bool(value)


def _uses_status(tree):
    """Whether an expression calls a status function, which suppresses the implicit `success() &&`."""
    if tree[0] == "call":
        return tree[1] in STATUS_FUNCTIONS or any(_uses_status(argument) for argument in tree[2])
    return any(_uses_status(part) for part in tree[1:] if isinstance(part, tuple))


def evaluate_expression(tree, context, references):
    """Evaluate a parsed expression, short-circuiting as GitHub does; unmodelled forms raise."""
    kind = tree[0]
    if kind == "or":
        left = evaluate_expression(tree[1], context, references)
        return left if _truthy(left) else evaluate_expression(tree[2], context, references)
    if kind == "and":
        left = evaluate_expression(tree[1], context, references)
        return evaluate_expression(tree[2], context, references) if _truthy(left) else left
    if kind == "not":
        return not _truthy(evaluate_expression(tree[1], context, references))
    if kind in ("eq", "ne"):
        # GitHub compares strings case-insensitively.
        same = (str(evaluate_expression(tree[1], context, references)).lower()
                == str(evaluate_expression(tree[2], context, references)).lower())
        return same if kind == "eq" else not same
    if kind == "literal":
        return tree[1]
    if kind == "call":
        if tree[1] in ("always", "success") and not tree[2]:
            return context["status"][tree[1]]
        arguments = [evaluate_expression(argument, context, references) for argument in tree[2]]
        if tree[1] == "contains" and len(arguments) == 2:
            # An array contains an equal item; a string contains a substring; both case-insensitively.
            needle = str(arguments[1]).lower()
            if isinstance(arguments[0], list):
                return any(str(item).lower() == needle for item in arguments[0])
            return needle in str(arguments[0]).lower()
        if tree[1] == "format" and arguments:
            result = str(arguments[0])
            for index, argument in enumerate(arguments[1:]):
                result = result.replace("{" + str(index) + "}", str(argument))
            return result
        raise UnsupportedExpression(f"{tree[1]}() is not modelled")
    if not any(pattern.fullmatch(tree[1]) for pattern in references):
        raise UnsupportedExpression(f"{tree[1]} is not modelled here")
    return _lookup(context, tree[1].split("."))


def _lookup(value, parts):
    """Follow a dotted path; `*` maps the rest of the path over a list; a missing key reads as ""."""
    for position, part in enumerate(parts):
        if part == "*":
            items = value if isinstance(value, list) else []
            return [_lookup(item, parts[position + 1:]) for item in items]
        value = value.get(part, "") if isinstance(value, dict) else ""
    return value


def condition_holds(text, context, default_status):
    """Evaluate an `if:`; one without a status function is `success() && (...)`, as GitHub reads it."""
    if text in (None, ""):
        return default_status
    text = text.strip()
    if text.startswith("${{") and text.endswith("}}"):
        text = text[3:-2]
    tree = _ExpressionParser(text).parse()
    if not _uses_status(tree):
        tree = ("and", ("call", "success", []), tree)
    status = {"always": True, "success": default_status}
    return _truthy(evaluate_expression(tree, dict(context, status=status), IF_REFERENCES))


def substitute(text, context):
    """Replace every `${{ }}` in a value with its evaluation."""
    def replace(match):
        value = evaluate_expression(_ExpressionParser(match.group(1)).parse(), context, VALUE_REFERENCES)
        if isinstance(value, bool):
            return "true" if value else "false"
        return str(value)
    return re.sub(r"\$\{\{(.*?)\}\}", replace, text, flags=re.S)


def read_key_values(path):
    """The `name=value` lines a step appended to GITHUB_OUTPUT or GITHUB_ENV."""
    values = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if "=" in line:
            name, value = line.split("=", 1)
            values[name] = value
    return values
