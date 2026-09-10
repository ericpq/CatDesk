//! What CatDesk is doing, and what it has been doing.
//!
//! An MCP tool call is one request and one response, so a five-minute
//! `run_checks` produces nothing at all until it finishes. The widget renders
//! whatever arrived with the last response and then sits there, which reads as
//! a hang rather than as work in progress.
//!
//! Two things are kept here. The calls in flight, pushed to a reader the
//! moment they start and finish - most last a few milliseconds, so a reader
//! that samples sees almost none of them. And enough history that opening the
//! monitor mid-session shows the session, rather than starting from blank.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::broadcast;

/// Long enough to identify the work, short enough to stay a single line.
const MAX_DETAIL_CHARS: usize = 120;
/// Why a call failed, trimmed to something that fits a row.
const MAX_REASON_CHARS: usize = 160;
/// Recent calls kept in full. The monitor shows the newest few; the rest are
/// here so a glance back is possible without making the list a wall of rows.
/// The per-tool totals, not this list, are what summarise a session.
const MAX_RECENT: usize = 12;
/// Timestamps kept for the activity graph. At one call a second this is about
/// ten minutes, which is the span worth looking at on a monitor.
const MAX_HISTORY: usize = 600;
/// Deep enough to absorb a burst of fast calls before a slow reader lags.
const EVENT_BUFFER: usize = 256;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static ACTIVITY: LazyLock<Mutex<Activity>> = LazyLock::new(|| Mutex::new(Activity::default()));
static EVENTS: LazyLock<broadcast::Sender<String>> =
    LazyLock::new(|| broadcast::channel(EVENT_BUFFER).0);

/// The state at each change, for a reader that cannot afford to sample.
///
/// Most calls finish in a few milliseconds, so a once-a-second poll sees
/// almost none of them in flight. Each event also carries the snapshot taken
/// when it was emitted: a coalescing channel, or re-reading the state on
/// delivery, would show a call that had already finished by then.
pub fn subscribe() -> broadcast::Receiver<String> {
    EVENTS.subscribe()
}

fn notify() {
    // No subscribers is the normal case; the error is not interesting.
    let _ = EVENTS.send(snapshot().to_string());
}

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
    reason: Option<String>,
    duration_ms: u64,
    ok: bool,
    finished_ms: u64,
}

#[derive(Clone, Debug, Default)]
struct ToolStat {
    count: u64,
    failed: u64,
    total_ms: u64,
}

#[derive(Debug, Default)]
struct Activity {
    in_flight: Vec<InFlight>,
    recent: Vec<Finished>,
    /// `(finished_ms, ok)` only: the graph needs when and whether, not what.
    history: VecDeque<(u64, bool)>,
    by_tool: BTreeMap<String, ToolStat>,
    completed: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn shorten(value: &str, limit: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(limit).collect();
    out.push('…');
    out
}

/// Registers a call as in flight until it is dropped, so a tool that returns
/// early - or panics - cannot leave the monitor showing work that has stopped.
#[derive(Debug)]
pub struct Guard {
    id: u64,
    ok: bool,
    reason: Option<String>,
}

impl Guard {
    /// A failed call without a reason is just a red dot; the reason is what
    /// makes the row worth reading.
    pub fn set_result(&mut self, ok: bool, reason: Option<String>) {
        self.ok = ok;
        self.reason = reason.map(|value| shorten(&value, MAX_REASON_CHARS));
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        {
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
            let duration_ms = finished_ms.saturating_sub(entry.started_ms);

            let stat = activity.by_tool.entry(entry.tool.clone()).or_default();
            stat.count += 1;
            stat.total_ms += duration_ms;
            if !self.ok {
                stat.failed += 1;
            }

            activity.history.push_back((finished_ms, self.ok));
            while activity.history.len() > MAX_HISTORY {
                activity.history.pop_front();
            }

            activity.completed += 1;
            activity.recent.insert(
                0,
                Finished {
                    tool: entry.tool,
                    detail: entry.detail,
                    reason: self.reason.take(),
                    duration_ms,
                    ok: self.ok,
                    finished_ms,
                },
            );
            activity.recent.truncate(MAX_RECENT);
        }
        notify();
    }
}

pub fn begin(tool: &str, detail: Option<String>) -> Guard {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut activity) = ACTIVITY.lock() {
        activity.in_flight.push(InFlight {
            id,
            tool: tool.to_string(),
            detail: detail
                .as_deref()
                .map(|value| shorten(value, MAX_DETAIL_CHARS)),
            started_ms: now_ms(),
        });
    }
    notify();
    // Assume success: a handler that returns an error overwrites this, and a
    // dropped future never gets the chance to report either way.
    Guard {
        id,
        ok: true,
        reason: None,
    }
}

fn finished_json(entry: &Finished) -> Value {
    json!({
        "tool": entry.tool,
        "detail": entry.detail,
        "reason": entry.reason,
        "durationMs": entry.duration_ms,
        "ok": entry.ok,
        "finishedMs": entry.finished_ms,
    })
}

/// What is happening now, plus the last few calls. Sent on every change, so it
/// stays small enough to push at the rate calls actually arrive.
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
        "recent": activity.recent.iter().map(finished_json).collect::<Vec<_>>(),
    })
}

/// Everything the monitor needs to draw a session it was not watching: the
/// timestamps behind the activity graph and the per-tool totals. Too large to
/// push on every event, so it is fetched rather than streamed.
pub fn full_snapshot() -> Value {
    let mut value = snapshot();
    let Ok(activity) = ACTIVITY.lock() else {
        return value;
    };
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    object.insert(
        "history".to_string(),
        json!(
            activity
                .history
                .iter()
                .map(|(finished_ms, ok)| json!([finished_ms, ok]))
                .collect::<Vec<_>>()
        ),
    );
    let mut by_tool: Vec<(&String, &ToolStat)> = activity.by_tool.iter().collect();
    by_tool.sort_by(|left, right| right.1.count.cmp(&left.1.count));
    object.insert(
        "byTool".to_string(),
        json!(
            by_tool
                .iter()
                .map(|(tool, stat)| json!({
                    "tool": tool,
                    "count": stat.count,
                    "failed": stat.failed,
                    "totalMs": stat.total_ms,
                }))
                .collect::<Vec<_>>()
        ),
    );
    drop(activity);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One shared record, so the tests take turns rather than racing each
    /// other's counters.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn reset() {
        let mut activity = ACTIVITY.lock().expect("lock activity");
        activity.in_flight.clear();
        activity.recent.clear();
        activity.history.clear();
        activity.by_tool.clear();
        activity.completed = 0;
    }

    // These share one process-global record, so they run under a single test.
    #[test]
    fn activity_reports_running_work_and_then_its_result() {
        let _serial = serial();
        reset();
        let mut events = subscribe();

        assert_eq!(snapshot()["busy"], json!(false));

        let mut guard = begin("run_checks", Some("cargo test --release".to_string()));
        // A reader cannot poll for work this short, so it has to be told, and
        // the event has to carry the running state rather than a promise to
        // look it up later.
        let started = events.try_recv().expect("no start event");
        assert!(started.contains("\"busy\":true"), "start event: {started}");

        let running = snapshot();
        assert_eq!(running["running"][0]["tool"], json!("run_checks"));
        assert_eq!(
            running["running"][0]["detail"],
            json!("cargo test --release")
        );

        guard.set_result(false, Some("exit 101".to_string()));
        drop(guard);
        let finished = events.try_recv().expect("no finish event");
        assert!(
            finished.contains("\"busy\":false"),
            "finish event: {finished}"
        );

        let done = snapshot();
        assert_eq!(done["busy"], json!(false));
        assert_eq!(done["completed"], json!(1));
        assert_eq!(done["recent"][0]["tool"], json!("run_checks"));
        assert_eq!(done["recent"][0]["ok"], json!(false));
        assert_eq!(done["recent"][0]["reason"], json!("exit 101"));

        // A guard dropped without a verdict counts as having worked.
        drop(begin("read", None));
        let after_read = snapshot();
        assert_eq!(after_read["recent"][0]["tool"], json!("read"));
        assert_eq!(after_read["recent"][0]["ok"], json!(true));
        assert_eq!(after_read["recent"][0]["reason"], Value::Null);

        // Overlapping calls both show, oldest first.
        let outer = begin("start_command", Some("sleep 30".to_string()));
        let inner = begin("read", Some("src/mcp.rs".to_string()));
        let both = snapshot();
        assert_eq!(both["running"].as_array().map(Vec::len), Some(2));
        assert_eq!(both["running"][0]["tool"], json!("start_command"));
        drop(inner);
        drop(outer);
        assert_eq!(snapshot()["busy"], json!(false));

        // The graph and the per-tool list are what make an already-running
        // session readable when the monitor is opened part-way through.
        let full = full_snapshot();
        assert_eq!(full["history"].as_array().map(Vec::len), Some(4));
        let by_tool = full["byTool"].as_array().expect("byTool");
        assert_eq!(
            by_tool[0]["tool"],
            json!("read"),
            "busiest tool comes first"
        );
        assert_eq!(by_tool[0]["count"], json!(2));
        let run_checks = by_tool
            .iter()
            .find(|entry| entry["tool"] == json!("run_checks"))
            .expect("run_checks missing");
        assert_eq!(run_checks["failed"], json!(1));

        reset();
    }

    #[test]
    fn history_and_recent_are_both_bounded() {
        let _serial = serial();
        reset();
        for _ in 0..(MAX_RECENT + 5) {
            drop(begin("read", None));
        }
        let full = full_snapshot();

        assert_eq!(full["recent"].as_array().map(Vec::len), Some(MAX_RECENT));
        assert_eq!(
            full["history"].as_array().map(Vec::len),
            Some(MAX_RECENT + 5),
            "history outlives the detailed list"
        );
        assert_eq!(full["completed"], json!(MAX_RECENT as u64 + 5));

        reset();
    }

    #[test]
    fn a_long_detail_is_collapsed_to_one_line() {
        let detail = shorten(
            "  cargo   test\n  --release   --offline  ",
            MAX_DETAIL_CHARS,
        );
        assert_eq!(detail, "cargo test --release --offline");

        let long = shorten(&"x".repeat(MAX_DETAIL_CHARS + 50), MAX_DETAIL_CHARS);
        assert_eq!(long.chars().count(), MAX_DETAIL_CHARS + 1);
        assert!(long.ends_with('…'));
    }
}
