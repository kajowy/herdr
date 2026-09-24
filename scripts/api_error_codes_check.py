"""Check the socket API error code reference against the Rust sources.

Collects the error code string literals that `src/` hands to the JSON API
error envelope and compares them with the codes listed in the socket API
error code reference table, so a new code cannot ship undocumented.

Codes are collected syntactically, from these producer shapes:

1. a struct literal field named `code` (`ErrorBody { code: "..." }` and the
   private error carriers that mirror it),
2. a positional argument of a function whose parameter at that index is
   named `code` and typed as a string (`encode_error`, `error_response_json`,
   `ApiFailure::new`, ...); associated functions are matched qualified by
   their `impl` type so unrelated `new("...")` calls are not collected,
3. a positional argument of a file-local closure with a `code` parameter,
4. an `Err(("code", message))` tuple, the shape used by helpers that return
   `Result<_, (&'static str, String)>`,
5. the leading element of the tuples assigned by `let (code, message) = ...`,
6. every literal returned by a `*_code` function returning `&'static str`.

Each literal may be wrapped in `.into()`, `.to_string()` or `.to_owned()`.

`#[cfg(test)]` items and test-only files are stripped first, so codes that
only exist in assertions are not required in the docs.

Every file that produces a code must be classified as an API producer or as
a non-API producer; an unclassified file is an error, which is what keeps a
new error surface from silently bypassing the reference table.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


DEFAULT_SOURCE_ROOT = Path("src")
DEFAULT_REFERENCE = Path("docs/next/website/src/content/docs/socket-api.mdx")

# Translated socket API pages repeat the code column; descriptions stay in
# English there, the way the config reference page already does.
TRANSLATION_LOCALES = ("ja", "zh-cn")

# Files whose error codes reach a JSON API client through the socket API
# response envelope. Prefixes ending in "/" match a whole subtree.
API_SOURCES = (
    "src/api/",
    "src/app/",
    "src/server/headless.rs",
    "src/server/headless/notifications.rs",
)

# Files that build the same envelope shape but never answer a socket API
# request. Their codes stay out of the socket API reference on purpose.
NON_API_SOURCES = {
    # `herdr <command> --json` prints these locally when the request never
    # reaches a server, so no socket API client can observe them.
    "src/cli.rs": "CLI-only output",
    "src/cli/": "CLI-only output",
    # Client shell endpoint transport: these answer the private client
    # socket, not the socket API listener.
    "src/server/client_commands.rs": "client shell endpoint transport",
    "src/server/client_transport.rs": "client shell endpoint transport",
    "src/server/headless/endpoint_requests.rs": "client shell endpoint transport",
    # File upload destination validation: pure logic with no caller yet in this task; a later
    # task wires it into a socket API request, at which point this moves to API_SOURCES.
    "src/server/file_transfer/destination.rs": "not yet wired to a socket API request",
    # File upload transfer registry: begin/chunk/commit/abort logic with no caller yet in this
    # task; a later task wires it into a socket API request, at which point this moves to
    # API_SOURCES.
    "src/server/file_transfer.rs": "not yet wired to a socket API request",
}

CODE_LITERAL = r'"([a-z][a-z0-9_]*)"\s*(?:\.\s*(?:into|to_string|to_owned)\s*\(\s*\))?'
CLOSURE_RE = re.compile(r"\blet\s+([a-z_][a-z0-9_]*)\s*=\s*(?:move\s+)?\|([^|]*)\|")
CODE_FIELD_RE = re.compile(r'(?<![A-Za-z0-9_])"?code"?\s*:\s*' + CODE_LITERAL)
ERR_TUPLE_RE = re.compile(r"Err\s*\(\s*\(\s*" + CODE_LITERAL + r"\s*,")
CODE_BINDING_RE = re.compile(r"\blet\s*\(\s*code\s*,")
TUPLE_HEAD_RE = re.compile(r"\(\s*" + CODE_LITERAL + r"\s*,")
CODE_FN_RE = re.compile(r"\bfn\s+[a-z_]*code\s*\([^)]*\)\s*->\s*&'static str\s*\{", re.S)
CODE_LITERAL_ONLY_RE = re.compile(CODE_LITERAL + r"\Z")
FN_RE = re.compile(r"\bfn\s+([a-z_][a-z0-9_]*)\s*\(([^)]*)\)", re.S)
IMPL_RE = re.compile(r"^\s*impl(?:<[^>]*>)?\s+(?:[A-Za-z0-9_:<>, ']+\s+for\s+)?([A-Za-z0-9_]+)")
CFG_TEST_RE = re.compile(r"#\[cfg\((?:all\()?test[,)]")
STRING_TYPE_RE = re.compile(r"\bstr\b|\bString\b")

# Doc table rows look like `| `code` | when it happens |`.
DOC_ROW_RE = re.compile(r"^\|\s*`([a-z][a-z0-9_]*)`\s*\|", re.M)


def is_test_path(path: Path) -> bool:
    parts = path.parts
    return (
        "tests" in parts
        or path.name in {"tests.rs", "test_support.rs"}
        or path.name.endswith("_tests.rs")
    )


def strip_cfg_test(text: str) -> str:
    """Drop `#[cfg(test)]` items so test-only literals are not collected."""
    kept: list[str] = []
    index = 0
    while True:
        match = CFG_TEST_RE.search(text, index)
        if not match:
            kept.append(text[index:])
            return "".join(kept)
        kept.append(text[index : match.start()])
        open_brace = text.find("{", match.end())
        if open_brace < 0:
            return "".join(kept)
        index = skip_block(text, open_brace)


def skip_block(text: str, open_brace: int) -> int:
    """Return the index just past the block that starts at `open_brace`."""
    depth = 0
    for index in range(open_brace, len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                return index + 1
    return len(text)


def split_top_level(text: str) -> list[str]:
    parts: list[str] = []
    depth = 0
    current: list[str] = []
    for char in text:
        if char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
        if char == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(char)
    parts.append("".join(current))
    return [part.strip() for part in parts]


def impl_blocks(text: str) -> list[tuple[int, int, str]]:
    """Return (start, end, type name) for every `impl` block in the file."""
    blocks: list[tuple[int, int, str]] = []
    for match in re.finditer(r"^[ \t]*impl[^\n{]*\{", text, re.M):
        impl_match = IMPL_RE.match(match.group(0))
        if not impl_match:
            continue
        open_brace = match.end() - 1
        blocks.append((open_brace, skip_block(text, open_brace), impl_match.group(1)))
    return blocks


def code_parameter_functions(sources: dict[str, str]) -> dict[str, set[int]]:
    """Map a call pattern to the argument indexes that carry an error code.

    Associated functions are keyed as `Type::name` so that generic names such
    as `new` only match calls on the error carrier they belong to.
    """
    found: dict[str, set[int]] = {}
    for text in sources.values():
        blocks = impl_blocks(text)
        for fn_match in FN_RE.finditer(text):
            for index, parameter in enumerate(split_top_level(fn_match.group(2))):
                name, _, rust_type = parameter.partition(":")
                if name.strip() != "code" or not STRING_TYPE_RE.search(rust_type):
                    continue
                enclosing = [
                    impl_type
                    for start, end, impl_type in blocks
                    if start < fn_match.start() < end
                ]
                key = fn_match.group(1)
                if enclosing:
                    key = f"{enclosing[-1]}::{key}"
                found.setdefault(key, set()).add(index)
    return found


def call_argument_codes(text: str, pattern: str, indexes: set[int]) -> list[str]:
    call_re = re.compile(r"(?<![A-Za-z0-9_])" + re.escape(pattern) + r"\s*\(")
    codes: list[str] = []
    for match in call_re.finditer(text):
        open_paren = match.end() - 1
        end = skip_call(text, open_paren)
        arguments = split_top_level(text[open_paren + 1 : end])
        for index in indexes:
            if index >= len(arguments):
                continue
            literal = CODE_LITERAL_ONLY_RE.match(arguments[index].lstrip("&").strip())
            if literal:
                codes.append(literal.group(1))
    return codes


def statement_end(text: str, start: int) -> int:
    """Return the index of the `;` that ends the statement starting at `start`."""
    depth = 0
    for index in range(start, len(text)):
        char = text[index]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
        elif char == ";" and depth <= 0:
            return index
    return len(text)


def skip_call(text: str, open_paren: int) -> int:
    depth = 0
    for index in range(open_paren, len(text)):
        if text[index] == "(":
            depth += 1
        elif text[index] == ")":
            depth -= 1
            if depth == 0:
                return index
    return len(text)


def collect_codes(source_root: Path) -> dict[str, set[str]]:
    """Return every collected code mapped to the files that produce it."""
    sources = {
        (Path(source_root.name) / path.relative_to(source_root)).as_posix(): strip_cfg_test(
            path.read_text(encoding="utf-8")
        )
        for path in sorted(source_root.rglob("*.rs"))
        if not is_test_path(path)
    }
    code_functions = code_parameter_functions(sources)

    producers: dict[str, set[str]] = {}
    for name, text in sources.items():
        codes = [match.group(1) for match in CODE_FIELD_RE.finditer(text)]
        codes += [match.group(1) for match in ERR_TUPLE_RE.finditer(text)]
        for match in CODE_BINDING_RE.finditer(text):
            statement = text[match.start() : statement_end(text, match.end())]
            codes += [hit.group(1) for hit in TUPLE_HEAD_RE.finditer(statement)]
        for match in CODE_FN_RE.finditer(text):
            body = text[match.end() - 1 : skip_block(text, match.end() - 1)]
            codes += re.findall(r'"([a-z][a-z0-9_]*)"', body)
        callables = dict(code_functions)
        # Closures are file-local, and their parameters are usually untyped.
        for match in CLOSURE_RE.finditer(text):
            for index, parameter in enumerate(split_top_level(match.group(2))):
                if parameter.partition(":")[0].strip() == "code":
                    callables.setdefault(match.group(1), set()).add(index)
        for pattern, indexes in callables.items():
            codes += call_argument_codes(text, pattern, indexes)
        for code in codes:
            producers.setdefault(code, set()).add(name)
    return producers


def classify(path: str) -> str | None:
    """Return "api", the non-API reason, or None when the file is unknown."""
    for prefix, reason in NON_API_SOURCES.items():
        if path == prefix or (prefix.endswith("/") and path.startswith(prefix)):
            return reason
    for prefix in API_SOURCES:
        if path == prefix or (prefix.endswith("/") and path.startswith(prefix)):
            return "api"
    return None


def documented_codes(reference_path: Path) -> tuple[list[str], list[str]]:
    """Return the codes listed in a socket API page and any layout errors."""
    codes: list[str] = []
    errors: list[str] = []
    for table in reference_path.read_text(encoding="utf-8").split("\n\n"):
        rows = DOC_ROW_RE.findall(table)
        if not rows:
            continue
        if rows != sorted(rows):
            errors.append(f"{reference_path}: error code rows must be sorted within each table")
        codes.extend(rows)
    errors += [
        f"{code}: listed more than once in {reference_path}"
        for code in sorted({code for code in codes if codes.count(code) > 1})
    ]
    return codes, errors


def check(source_root: Path, reference_path: Path) -> list[str]:
    producers = collect_codes(source_root)
    documented, errors = documented_codes(reference_path)

    api_codes: set[str] = set()
    for code, paths in sorted(producers.items()):
        for path in sorted(paths):
            reason = classify(path)
            if reason is None:
                errors.append(
                    f"{path}: produces error code {code!r} but the file is not "
                    "classified; add it to API_SOURCES or NON_API_SOURCES"
                )
            elif reason == "api":
                api_codes.add(code)

    documented_set = set(documented)
    for missing in sorted(api_codes - documented_set):
        paths = ", ".join(sorted(producers[missing]))
        errors.append(f"{missing}: emitted by {paths} but missing from {reference_path}")
    for stale in sorted(documented_set - api_codes):
        errors.append(f"{stale}: listed in {reference_path} but no longer emitted by src/")

    for locale in TRANSLATION_LOCALES:
        translated_path = reference_path.parent / locale / reference_path.name
        if not translated_path.exists():
            continue
        translated, translation_errors = documented_codes(translated_path)
        errors += translation_errors
        if sorted(set(translated)) != sorted(documented_set):
            errors.append(
                f"{translated_path}: error code list differs from {reference_path}"
            )
    return errors


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Check the socket API error code reference against src/."
    )
    parser.add_argument("--source-root", default=DEFAULT_SOURCE_ROOT, type=Path)
    parser.add_argument("--reference", default=DEFAULT_REFERENCE, type=Path)
    parser.add_argument(
        "--emit",
        action="store_true",
        help="Print the collected API error codes with their producing files and exit.",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)

    if args.emit:
        for code, paths in sorted(collect_codes(args.source_root).items()):
            reasons = {classify(path) or "UNCLASSIFIED" for path in paths}
            kind = "api" if "api" in reasons else ", ".join(sorted(reasons))
            print(f"{code}\t{kind}\t{', '.join(sorted(paths))}")
        return 0

    errors = check(args.source_root, args.reference)
    if errors:
        print("error: socket API error code reference is out of sync with src/", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
