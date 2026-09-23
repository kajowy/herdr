from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from scripts.api_error_codes_check import (
    DEFAULT_REFERENCE,
    DEFAULT_SOURCE_ROOT,
    check,
    collect_codes,
)


SAMPLE_API_SOURCE = """
use crate::api::schema::ErrorBody;

fn encode_error(id: String, code: &str, message: impl Into<String>) -> String {
    format!("{id}{code}")
}

fn reject(id: String) -> String {
    encode_error(id, "pane_not_found", "pane not found")
}

fn body() -> ErrorBody {
    ErrorBody {
        code: "tab_not_found".into(),
        message: "tab not found".into(),
    }
}

fn tuple_failure() -> Result<(), (&'static str, String)> {
    Err(("invalid_env", "env key must not be empty".to_string()))
}

impl ApiFailure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

fn absolute() -> Result<(), ApiFailure> {
    Err(ApiFailure::new("invalid_request", "path must be absolute"))
}

fn closure_failure(id: &str) -> String {
    let error = |code, message: &str| encode_error(id.to_owned(), code, message);
    error("stale_target", "pane is no longer visible")
}

fn refusal_code(reason: &str) -> &'static str {
    if reason.contains("not found") {
        "terminal_not_found"
    } else {
        "attach_failed"
    }
}

fn binding_failure() -> (&'static str, String) {
    let (code, message) = match kind {
        Kind::Status => ("timeout", "timed out".to_string()),
        Kind::Stalled => ("agent_prompt_stalled", "no state change".to_string()),
    };
    (code, message)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects() {
        assert_eq!(response["error"]["code"], "test_only_code");
    }
}
"""

SAMPLE_CLI_SOURCE = """
fn print_session_error(code: &str, message: &str) {
    println!("{code} {message}");
}

fn missing() {
    print_session_error("server_not_running", "no server");
}
"""

SAMPLE_CODES = [
    "agent_prompt_stalled",
    "attach_failed",
    "invalid_env",
    "invalid_request",
    "pane_not_found",
    "stale_target",
    "tab_not_found",
    "terminal_not_found",
    "timeout",
]


def reference_page(codes: list[str]) -> str:
    rows = "\n".join(f"| `{code}` | Because. |" for code in codes)
    return f"""---
title: Socket API
---

### Error codes

**Everything**

| Code | When it occurs |
| --- | --- |
{rows}
"""


class CollectCodesTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name) / "src"
        (root / "api").mkdir(parents=True)
        (root / "cli").mkdir()
        (root / "api" / "server.rs").write_text(SAMPLE_API_SOURCE, encoding="utf-8")
        (root / "cli" / "session.rs").write_text(SAMPLE_CLI_SOURCE, encoding="utf-8")
        self.root = root

    def collected(self) -> dict[str, set[str]]:
        return collect_codes(self.root)

    def test_collects_every_producer_shape(self) -> None:
        self.assertEqual(sorted(self.collected()), sorted(SAMPLE_CODES + ["server_not_running"]))

    def test_ignores_cfg_test_literals(self) -> None:
        self.assertNotIn("test_only_code", self.collected())

    def test_records_the_producing_file(self) -> None:
        self.assertEqual(
            self.collected()["pane_not_found"],
            {"src/api/server.rs"},
        )


class CheckTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name)
        (root / "src" / "api").mkdir(parents=True)
        (root / "src" / "api" / "server.rs").write_text(SAMPLE_API_SOURCE, encoding="utf-8")
        (root / "src" / "cli").mkdir()
        (root / "src" / "cli" / "session.rs").write_text(SAMPLE_CLI_SOURCE, encoding="utf-8")
        self.root = root

    def run_check(self, codes: list[str]) -> list[str]:
        reference = self.root / "socket-api.mdx"
        reference.write_text(reference_page(codes), encoding="utf-8")
        return check(self.root / "src", reference)

    def test_in_sync_reference_passes(self) -> None:
        self.assertEqual(self.run_check(SAMPLE_CODES), [])

    def test_cli_only_code_is_not_required(self) -> None:
        self.assertNotIn("server_not_running", " ".join(self.run_check(SAMPLE_CODES)))

    def test_missing_code_is_named(self) -> None:
        errors = self.run_check([code for code in SAMPLE_CODES if code != "stale_target"])

        self.assertEqual(len(errors), 1)
        self.assertIn("stale_target", errors[0])
        self.assertIn("missing from", errors[0])

    def test_stale_code_is_named(self) -> None:
        errors = self.run_check(sorted(SAMPLE_CODES + ["removed_code"]))

        self.assertEqual(len(errors), 1)
        self.assertIn("removed_code", errors[0])
        self.assertIn("no longer emitted", errors[0])

    def test_duplicate_row_is_rejected(self) -> None:
        errors = self.run_check(sorted(SAMPLE_CODES + ["timeout"]))

        self.assertEqual(len(errors), 1)
        self.assertIn("listed more than once", errors[0])

    def test_unsorted_rows_are_rejected(self) -> None:
        errors = self.run_check(list(reversed(SAMPLE_CODES)))

        self.assertEqual(len(errors), 1)
        self.assertIn("sorted", errors[0])

    def test_unclassified_producer_is_rejected(self) -> None:
        (self.root / "src" / "detect").mkdir()
        (self.root / "src" / "detect" / "surprise.rs").write_text(
            'fn body() -> ErrorBody { ErrorBody { code: "surprise".into() } }\n',
            encoding="utf-8",
        )

        errors = self.run_check(SAMPLE_CODES)

        self.assertEqual(len(errors), 1)
        self.assertIn("not classified", errors[0])

    def test_translation_with_a_different_list_is_rejected(self) -> None:
        locale = self.root / "ja"
        locale.mkdir()
        (locale / "socket-api.mdx").write_text(
            reference_page([code for code in SAMPLE_CODES if code != "timeout"]),
            encoding="utf-8",
        )

        errors = self.run_check(SAMPLE_CODES)

        self.assertEqual(len(errors), 1)
        self.assertIn("differs from", errors[0])


class RealSourceTests(unittest.TestCase):
    def test_real_sources_yield_classified_codes(self) -> None:
        codes = collect_codes(DEFAULT_SOURCE_ROOT)

        self.assertGreater(len(codes), 100)
        self.assertIn("pane_not_found", codes)
        self.assertIn("tab_not_found", codes)

    def test_next_docs_match_the_real_sources(self) -> None:
        self.assertEqual(check(DEFAULT_SOURCE_ROOT, DEFAULT_REFERENCE), [])


if __name__ == "__main__":
    unittest.main()
