//! Turn a test, build or type-check run into a structured result.
//!
//! Running the checks through `run_command` does not work: its buffer keeps the
//! first 32 KiB and a test summary is the last thing a runner prints, so the
//! answer is exactly the part that gets dropped. This module runs the check
//! with a large buffer and hands back counts, failing test names and the
//! file/line of each diagnostic, so the caller can act without reading a
//! transcript it would have to page through.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

/// How much of the runner's output to keep while parsing. Large enough for a
/// full workspace test run; the caller never sees more than `OUTPUT_TAIL_BYTES`.
pub const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;
/// Summaries and the last failure land at the end, so keep the tail.
pub const OUTPUT_TAIL_BYTES: usize = 4 * 1024;
pub const DEFAULT_TIMEOUT_MS: u64 = 300_000;
pub const MAX_TIMEOUT_MS: u64 = 900_000;
const MAX_FAILURES: usize = 50;
const MAX_DIAGNOSTICS: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckKind {
    Cargo,
    Pytest,
    Go,
    Node,
    Generic,
}

impl CheckKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckKind::Cargo => "cargo",
            CheckKind::Pytest => "pytest",
            CheckKind::Go => "go",
            CheckKind::Node => "node",
            CheckKind::Generic => "generic",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cargo" | "rust" => Some(CheckKind::Cargo),
            "pytest" | "python" => Some(CheckKind::Pytest),
            "go" | "golang" => Some(CheckKind::Go),
            "node" | "npm" | "javascript" | "typescript" => Some(CheckKind::Node),
            "generic" | "shell" => Some(CheckKind::Generic),
            _ => None,
        }
    }

    /// Pick the toolchain from the marker files a project keeps at its root.
    /// Cargo and Go come first because their markers are unambiguous.
    pub fn detect(dir: &Path) -> Self {
        if dir.join("Cargo.toml").is_file() {
            return CheckKind::Cargo;
        }
        if dir.join("go.mod").is_file() {
            return CheckKind::Go;
        }
        for marker in ["pytest.ini", "tox.ini", "setup.cfg", "pyproject.toml"] {
            if dir.join(marker).is_file() {
                return CheckKind::Pytest;
            }
        }
        if dir.join("package.json").is_file() {
            return CheckKind::Node;
        }
        if dir.join("tests").is_dir() || dir.join("test").is_dir() {
            return CheckKind::Pytest;
        }
        CheckKind::Generic
    }

    pub fn default_command(self) -> Option<&'static str> {
        match self {
            CheckKind::Cargo => Some("cargo test"),
            CheckKind::Pytest => Some("python3 -m pytest -q"),
            CheckKind::Go => Some("go test ./..."),
            CheckKind::Node => Some("npm test"),
            CheckKind::Generic => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Failure {
    pub name: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub severity: String,
    pub file: String,
    pub line: u32,
    pub column: Option<u32>,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckOutcome {
    pub passed: Option<u32>,
    pub failed: Option<u32>,
    pub failures: Vec<Failure>,
    pub diagnostics: Vec<Diagnostic>,
    pub summary: Option<String>,
}

impl CheckOutcome {
    fn trim(mut self) -> Self {
        self.failures.truncate(MAX_FAILURES);
        self.diagnostics.truncate(MAX_DIAGNOSTICS);
        self
    }
}

/// Keep the last `OUTPUT_TAIL_BYTES` of `text` on a character boundary.
pub fn output_tail(text: &str) -> (String, bool) {
    if text.len() <= OUTPUT_TAIL_BYTES {
        return (text.to_string(), false);
    }
    let mut start = text.len() - OUTPUT_TAIL_BYTES;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_string(), true)
}

pub fn parse(kind: CheckKind, stdout: &str, stderr: &str) -> CheckOutcome {
    // Runners split themselves across both streams: cargo prints compiler
    // diagnostics on stderr and test results on stdout, pytest does the
    // reverse under some plugins. Parse the pair as one transcript.
    let combined = if stderr.is_empty() {
        stdout.to_string()
    } else if stdout.is_empty() {
        stderr.to_string()
    } else {
        format!("{stdout}\n{stderr}")
    };

    let outcome = match kind {
        CheckKind::Cargo => parse_cargo(&combined),
        CheckKind::Pytest => parse_pytest(&combined),
        CheckKind::Go => parse_go(&combined),
        CheckKind::Node => parse_node(&combined),
        CheckKind::Generic => CheckOutcome::default(),
    };
    outcome.trim()
}

static CARGO_SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed").expect("cargo summary")
});
static CARGO_FAILURE_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^---- (.+) stdout ----$").expect("cargo failure header"));
static CARGO_PANIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"panicked at (.+?):(\d+):(\d+):").expect("cargo panic"));
static RUSTC_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*--> (.+?):(\d+):(\d+)$").expect("rustc location"));

fn parse_cargo(text: &str) -> CheckOutcome {
    let mut outcome = CheckOutcome::default();
    let (mut passed, mut failed) = (0u32, 0u32);
    let mut saw_summary = false;

    for capture in CARGO_SUMMARY.captures_iter(text) {
        // A workspace prints one summary per test binary; add them up.
        saw_summary = true;
        passed += capture[1].parse::<u32>().unwrap_or(0);
        failed += capture[2].parse::<u32>().unwrap_or(0);
    }
    if saw_summary {
        outcome.passed = Some(passed);
        outcome.failed = Some(failed);
        outcome.summary = text
            .lines()
            .rev()
            .find(|line| line.starts_with("test result:"))
            .map(str::to_string);
    }

    let lines: Vec<&str> = text.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let Some(header) = CARGO_FAILURE_HEADER.captures(lines[index]) else {
            index += 1;
            continue;
        };
        let mut failure = Failure {
            name: header[1].to_string(),
            ..Failure::default()
        };
        index += 1;
        let mut message: Option<String> = None;
        while index < lines.len() && !CARGO_FAILURE_HEADER.is_match(lines[index]) {
            let line = lines[index];
            if line.starts_with("failures:") {
                break;
            }
            if let Some(panic) = CARGO_PANIC.captures(line) {
                failure.file = Some(panic[1].to_string());
                failure.line = panic[2].parse().ok();
                // The assertion text is on the lines after the panic banner.
                message = lines
                    .get(index + 1..)
                    .and_then(|rest| {
                        rest.iter()
                            .take_while(|next| !CARGO_FAILURE_HEADER.is_match(next))
                            .find(|next| !next.trim().is_empty() && !next.starts_with("note:"))
                    })
                    .map(|line| line.trim().to_string());
            } else if message.is_none()
                && !line.trim().is_empty()
                && !line.starts_with("thread ")
                && !line.starts_with("note:")
            {
                message = Some(line.trim().to_string());
            }
            index += 1;
        }
        failure.message = message;
        outcome.failures.push(failure);
    }

    outcome.diagnostics = parse_rustc_diagnostics(&lines);
    outcome
}

fn parse_rustc_diagnostics(lines: &[&str]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let severity = if line.starts_with("error") {
            "error"
        } else if line.starts_with("warning") {
            "warning"
        } else {
            continue;
        };
        let Some((_, message)) = line.split_once(": ") else {
            continue;
        };
        // Only report a diagnostic that carries a location. Summary lines such
        // as "error: test failed, to rerun pass ..." have none and would only
        // repeat what the exit code already says.
        let location = lines
            .get(index + 1..(index + 4).min(lines.len()))
            .and_then(|window| window.iter().find_map(|next| RUSTC_LOCATION.captures(next)));
        let Some(location) = location else {
            continue;
        };
        diagnostics.push(Diagnostic {
            severity: severity.to_string(),
            file: location[1].to_string(),
            line: location[2].parse().unwrap_or(0),
            column: location[3].parse().ok(),
            message: message.trim().to_string(),
        });
    }
    diagnostics
}

static PYTEST_COUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d+) (passed|failed|error|errors|skipped|xfailed|xpassed)")
        .expect("pytest counts")
});
static PYTEST_FAILURE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:FAILED|ERROR) (\S+)(?: - (.*))?$").expect("pytest failure"));

fn parse_pytest(text: &str) -> CheckOutcome {
    let mut outcome = CheckOutcome::default();

    // The last line carrying counts is pytest's own final summary; earlier
    // matches can come from a plugin's per-file progress output.
    if let Some(summary) = text
        .lines()
        .rev()
        .find(|line| {
            line.contains(" passed") || line.contains(" failed") || line.contains(" error")
        })
        .filter(|line| PYTEST_COUNT.is_match(line))
    {
        let (mut passed, mut failed) = (0u32, 0u32);
        for capture in PYTEST_COUNT.captures_iter(summary) {
            let count = capture[1].parse::<u32>().unwrap_or(0);
            match &capture[2] {
                "passed" | "xpassed" => passed += count,
                "failed" | "error" | "errors" => failed += count,
                _ => {}
            }
        }
        outcome.passed = Some(passed);
        outcome.failed = Some(failed);
        outcome.summary = Some(summary.trim().trim_matches('=').trim().to_string());
    }

    for line in text.lines() {
        let Some(capture) = PYTEST_FAILURE.captures(line) else {
            continue;
        };
        let name = capture[1].to_string();
        outcome.failures.push(Failure {
            file: name.split("::").next().map(str::to_string),
            line: None,
            message: capture.get(2).map(|value| value.as_str().to_string()),
            name,
        });
    }

    outcome
}

static GO_FAILURE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*--- FAIL: (\S+)").expect("go failure"));
static GO_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+(\S+\.go):(\d+): (.*)$").expect("go location"));

fn parse_go(text: &str) -> CheckOutcome {
    let mut outcome = CheckOutcome::default();
    let mut passed = 0u32;

    for line in text.lines() {
        if line.starts_with("--- PASS") || line.trim_start().starts_with("--- PASS") {
            passed += 1;
            continue;
        }
        if let Some(capture) = GO_FAILURE.captures(line) {
            outcome.failures.push(Failure {
                name: capture[1].to_string(),
                ..Failure::default()
            });
            continue;
        }
        // A location line belongs to the failure it is printed under.
        if let Some(capture) = GO_LOCATION.captures(line)
            && let Some(failure) = outcome.failures.last_mut()
            && failure.file.is_none()
        {
            failure.file = Some(capture[1].to_string());
            failure.line = capture[2].parse().ok();
            failure.message = Some(capture[3].trim().to_string());
        }
    }

    // `go test` prints no totals, so report what the transcript actually shows.
    outcome.failed = Some(outcome.failures.len() as u32);
    if passed > 0 {
        outcome.passed = Some(passed);
    }
    outcome.summary = text
        .lines()
        .rev()
        .find(|line| line.starts_with("FAIL") || line.starts_with("ok "))
        .map(|line| line.trim().to_string());
    outcome
}

static NODE_SUMMARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Tests:?\s+(.*)$").expect("node summary"));
static NODE_COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+) (passed|failed)").expect("node counts"));
static NODE_FAILURE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:✕|×|●|✗)\s+(.+?)\s*$").expect("node failure"));
static TSC_DIAGNOSTIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+?)\((\d+),(\d+)\): (error|warning) TS\d+: (.*)$").expect("tsc diagnostic")
});

fn parse_node(text: &str) -> CheckOutcome {
    let mut outcome = CheckOutcome::default();

    if let Some(summary) = text
        .lines()
        .rev()
        .find(|line| NODE_SUMMARY.is_match(line) && NODE_COUNT.is_match(line))
    {
        let (mut passed, mut failed) = (0u32, 0u32);
        for capture in NODE_COUNT.captures_iter(summary) {
            let count = capture[1].parse::<u32>().unwrap_or(0);
            match &capture[2] {
                "passed" => passed += count,
                _ => failed += count,
            }
        }
        outcome.passed = Some(passed);
        outcome.failed = Some(failed);
        outcome.summary = Some(summary.trim().to_string());
    }

    for line in text.lines() {
        if let Some(capture) = TSC_DIAGNOSTIC.captures(line) {
            outcome.diagnostics.push(Diagnostic {
                severity: capture[4].to_string(),
                file: capture[1].to_string(),
                line: capture[2].parse().unwrap_or(0),
                column: capture[3].parse().ok(),
                message: capture[5].trim().to_string(),
            });
            continue;
        }
        if let Some(capture) = NODE_FAILURE.captures(line) {
            outcome.failures.push(Failure {
                name: capture[1].trim().to_string(),
                ..Failure::default()
            });
        }
    }

    // A type-check run reports no test totals; the diagnostics are the result.
    if outcome.passed.is_none() && !outcome.diagnostics.is_empty() {
        outcome.failed = Some(
            outcome
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == "error")
                .count() as u32,
        );
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_run_reports_counts_the_failing_test_and_its_assertion() {
        let text = "\
running 227 tests
test command::tests::ok_one ... ok

failures:

---- linux_sandbox::tests::runtime_read_paths_do_not_grant_the_home_directory_itself stdout ----

thread 'linux_sandbox::tests::runtime_read_paths_do_not_grant_the_home_directory_itself' (117) panicked at src/linux_sandbox.rs:292:9:
assertion failed: !runtime_read_paths().contains(&home)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

test result: FAILED. 226 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
";
        let outcome = parse(CheckKind::Cargo, text, "");

        assert_eq!(outcome.passed, Some(226));
        assert_eq!(outcome.failed, Some(1));
        assert_eq!(outcome.failures.len(), 1);
        let failure = &outcome.failures[0];
        assert_eq!(
            failure.name,
            "linux_sandbox::tests::runtime_read_paths_do_not_grant_the_home_directory_itself"
        );
        assert_eq!(failure.file.as_deref(), Some("src/linux_sandbox.rs"));
        assert_eq!(failure.line, Some(292));
        assert_eq!(
            failure.message.as_deref(),
            Some("assertion failed: !runtime_read_paths().contains(&home)")
        );
    }

    #[test]
    fn cargo_workspace_totals_add_up_over_every_test_binary() {
        let text = "\
test result: ok. 10 passed; 0 failed; 0 ignored
test result: FAILED. 5 passed; 2 failed; 0 ignored
";
        let outcome = parse(CheckKind::Cargo, text, "");

        assert_eq!(outcome.passed, Some(15));
        assert_eq!(outcome.failed, Some(2));
    }

    #[test]
    fn cargo_compile_errors_keep_only_the_ones_that_carry_a_location() {
        let stderr = "\
error[E0425]: cannot find value `nope` in this scope
  --> src/mcp.rs:1822:5
   |
error: could not compile `catdesk` (bin \"catdesk\") due to 1 previous error
";
        let outcome = parse(CheckKind::Cargo, "", stderr);

        assert_eq!(outcome.diagnostics.len(), 1);
        let diagnostic = &outcome.diagnostics[0];
        assert_eq!(diagnostic.severity, "error");
        assert_eq!(diagnostic.file, "src/mcp.rs");
        assert_eq!(diagnostic.line, 1822);
        assert_eq!(diagnostic.column, Some(5));
        assert_eq!(diagnostic.message, "cannot find value `nope` in this scope");
    }

    #[test]
    fn pytest_run_reports_counts_and_the_failing_node_ids() {
        let text = "\
FAILED tests/test_billing.py::test_refund - AssertionError: expected 0
ERROR tests/test_setup.py::test_fixture
==================== 1 failed, 1 error, 12 passed in 3.41s ====================
";
        let outcome = parse(CheckKind::Pytest, text, "");

        assert_eq!(outcome.passed, Some(12));
        assert_eq!(outcome.failed, Some(2));
        assert_eq!(outcome.failures.len(), 2);
        assert_eq!(
            outcome.failures[0].name,
            "tests/test_billing.py::test_refund"
        );
        assert_eq!(
            outcome.failures[0].file.as_deref(),
            Some("tests/test_billing.py")
        );
        assert_eq!(
            outcome.failures[0].message.as_deref(),
            Some("AssertionError: expected 0")
        );
        assert_eq!(outcome.failures[1].message, None);
    }

    #[test]
    fn go_run_attributes_a_location_to_the_failure_it_sits_under() {
        let text = "\
--- PASS: TestAdd (0.00s)
--- FAIL: TestRefund (0.01s)
    billing_test.go:42: expected 0, got 3
FAIL\texample.com/billing\t0.012s
";
        let outcome = parse(CheckKind::Go, text, "");

        assert_eq!(outcome.passed, Some(1));
        assert_eq!(outcome.failed, Some(1));
        assert_eq!(outcome.failures[0].name, "TestRefund");
        assert_eq!(outcome.failures[0].file.as_deref(), Some("billing_test.go"));
        assert_eq!(outcome.failures[0].line, Some(42));
        assert_eq!(
            outcome.failures[0].message.as_deref(),
            Some("expected 0, got 3")
        );
    }

    #[test]
    fn node_run_reads_jest_totals_and_typescript_diagnostics() {
        let text = "\
  ✕ refunds a charge
src/billing.ts(12,5): error TS2345: Argument of type 'string' is not assignable
Tests:       1 failed, 5 passed, 6 total
";
        let outcome = parse(CheckKind::Node, text, "");

        assert_eq!(outcome.passed, Some(5));
        assert_eq!(outcome.failed, Some(1));
        assert_eq!(outcome.failures[0].name, "refunds a charge");
        assert_eq!(outcome.diagnostics.len(), 1);
        assert_eq!(outcome.diagnostics[0].file, "src/billing.ts");
        assert_eq!(outcome.diagnostics[0].line, 12);
    }

    #[test]
    fn a_type_check_with_no_test_totals_still_reports_a_failure_count() {
        let text = "src/a.ts(1,1): error TS1005: ';' expected\n";
        let outcome = parse(CheckKind::Node, text, "");

        assert_eq!(outcome.passed, None);
        assert_eq!(outcome.failed, Some(1));
    }

    #[test]
    fn the_tail_is_kept_on_a_character_boundary() {
        let text = format!("{}三", "x".repeat(OUTPUT_TAIL_BYTES));
        let (tail, truncated) = output_tail(&text);

        assert!(truncated);
        assert!(tail.ends_with('三'));
        assert!(tail.len() <= OUTPUT_TAIL_BYTES);
    }

    #[test]
    fn a_short_transcript_is_not_reported_as_truncated() {
        let (tail, truncated) = output_tail("all good\n");

        assert_eq!(tail, "all good\n");
        assert!(!truncated);
    }

    #[test]
    fn detection_prefers_an_unambiguous_toolchain_marker() {
        let root =
            std::env::temp_dir().join(format!("catdesk-checks-detect-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create dir");
        std::fs::write(root.join("package.json"), "{}").expect("write package.json");
        assert_eq!(CheckKind::detect(&root), CheckKind::Node);

        std::fs::write(root.join("Cargo.toml"), "[package]").expect("write Cargo.toml");
        assert_eq!(CheckKind::detect(&root), CheckKind::Cargo);

        let _ = std::fs::remove_dir_all(root);
    }
}
