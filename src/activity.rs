//! What CatDesk is doing right now, for the widget to poll.
//!
//! An MCP tool call is one request and one response, so a five-minute
//! `run_checks` produces nothing at all until it finishes. The widget renders
//! whatever arrived with the last response and then sits there, which reads as
//! a hang rather than as work in progress.
//!
//! This is a small process-global record of the calls currently in flight. The
//! widget fetches it directly on a timer, so progress costs no tokens, no tool
//! calls, and no model involvement.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Long enough to identify the work, short enough to stay a single line.
const MAX_DETAIL_CHARS: usize = 120;
/// Keep the recent history shallow: this is a status strip, not a log.
const MAX_RECENT: usize = 5;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static ACTIVITY: LazyLock<Mutex<Activity>> = LazyLock::new(|| Mutex::new(Activity::default()));

#[derive(Clone, Debug)]
struct InFlight {
    id: u64,
    tool: String,
    detail: Option<String>,
    started_ms: u64,
}

#[derive(Clone, Debug)]
struct Finished {
    tool: String,
    detail: Option<String>,
    duration_ms: u64,
    ok: bool,
    finished_ms: u64,
}

#[derive(Debug, Default)]
struct Activity {
    in_flight: Vec<InFlight>,
    recent: Vec<Finished>,
    completed: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn shorten(value: &str) -> String {
    let value = value.trim();
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_DETAIL_CHARS {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(MAX_DETAIL_CHARS).collect();
    out.push('…');
    out
}

/// Registers a call as in flight until it is dropped, so a tool that returns
/// early - or panics - cannot leave the widget showing work that has stopped.
#[derive(Debug)]
pub struct Guard {
    id: u64,
    ok: bool,
}

impl Guard {
    pub fn set_ok(&mut self, ok: bool) {
        self.ok = ok;
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let Ok(mut activity) = ACTIVITY.lock() else {
            return;
        };
        let Some(index) = activity
            .in_flight
            .iter()
            .position(|entry| entry.id == self.id)
        else {
            return;
        };
        let entry = activity.in_flight.remove(index);
        let finished_ms = now_ms();
        let finished = Finished {
            duration_ms: finished_ms.saturating_sub(entry.started_ms),
            tool: entry.tool,
            detail: entry.detail,
            ok: self.ok,
            finished_ms,
        };
        activity.completed += 1;
        activity.recent.insert(0, finished);
        activity.recent.truncate(MAX_RECENT);
    }
}

pub fn begin(tool: &str, detail: Option<String>) -> Guard {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut activity) = ACTIVITY.lock() {
        activity.in_flight.push(InFlight {
            id,
            tool: tool.to_string(),
            detail: detail.as_deref().map(shorten),
            started_ms: now_ms(),
        });
    }
    // Assume success: a handler that returns an error overwrites this, and a
    // dropped future never gets the chance to report either way.
    Guard { id, ok: true }
}

pub fn snapshot() -> Value {
    let Ok(activity) = ACTIVITY.lock() else {
        return json!({ "busy": false, "running": [], "recent": [], "completed": 0 });
    };
    let now = now_ms();
    // Oldest first: the call that has been waiting longest is the one worth
    // showing when several overlap.
    let mut running: Vec<&InFlight> = activity.in_flight.iter().collect();
    running.sort_by_key(|entry| entry.started_ms);

    json!({
        "busy": !running.is_empty(),
        "serverTimeMs": now,
        "completed": activity.completed,
        "running": running
            .iter()
            .map(|entry| json!({
                "tool": entry.tool,
                "detail": entry.detail,
                "startedMs": entry.started_ms,
                "elapsedMs": now.saturating_sub(entry.started_ms),
            }))
            .collect::<Vec<_>>(),
        "recent": activity
            .recent
            .iter()
            .map(|entry| json!({
                "tool": entry.tool,
                "detail": entry.detail,
                "durationMs": entry.duration_ms,
                "ok": entry.ok,
                "finishedMs": entry.finished_ms,
            }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        let mut activity = ACTIVITY.lock().expect("lock activity");
        activity.in_flight.clear();
        activity.recent.clear();
        activity.completed = 0;
    }

    // These share one process-global record, so they run under a single test.
    #[test]
    fn activity_reports_running_work_and_then_its_result() {
        reset();

        assert_eq!(snapshot()["busy"], json!(false));

        let mut guard = begin("run_checks", Some("cargo test --release".to_string()));
        let running = snapshot();
        assert_eq!(running["busy"], json!(true));
        assert_eq!(running["running"][0]["tool"], json!("run_checks"));
        assert_eq!(
            running["running"][0]["detail"],
            json!("cargo test --release")
        );

        guard.set_ok(false);
        drop(guard);

        let done = snapshot();
        assert_eq!(done["busy"], json!(false));
        assert_eq!(done["completed"], json!(1));
        assert_eq!(done["recent"][0]["tool"], json!("run_checks"));
        assert_eq!(done["recent"][0]["ok"], json!(false));

        // A guard that is dropped without a verdict counts as having worked.
        drop(begin("read", None));
        let after_read = snapshot();
        assert_eq!(after_read["busy"], json!(false));
        assert_eq!(after_read["recent"][0]["tool"], json!("read"));
        assert_eq!(after_read["recent"][0]["ok"], json!(true));
        assert_eq!(after_read["recent"][0]["detail"], Value::Null);

        // Overlapping calls both show, oldest first.
        let outer = begin("start_command", Some("sleep 30".to_string()));
        let inner = begin("read", Some("src/mcp.rs".to_string()));
        let both = snapshot();
        assert_eq!(both["running"].as_array().map(Vec::len), Some(2));
        assert_eq!(both["running"][0]["tool"], json!("start_command"));
        drop(inner);
        assert_eq!(snapshot()["running"].as_array().map(Vec::len), Some(1));
        drop(outer);
        assert_eq!(snapshot()["busy"], json!(false));

        reset();
    }

    #[test]
    fn a_long_detail_is_collapsed_to_one_line() {
        let detail = shorten("  cargo   test\n  --release   --offline  ");
        assert_eq!(detail, "cargo test --release --offline");

        let long = shorten(&"x".repeat(MAX_DETAIL_CHARS + 50));
        assert_eq!(long.chars().count(), MAX_DETAIL_CHARS + 1);
        assert!(long.ends_with('…'));
    }
}
