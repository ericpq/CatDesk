use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const STATE_PATH: &str = ".catdesk/agent_state.json";
const AUDIT_PATH: &str = ".catdesk/agent_audit.jsonl";
const MAX_AUDIT_BYTES: u64 = 2 * 1024 * 1024;
const KEEP_AUDIT_LINES: usize = 1000;
const RECENT_AUDIT_LINES: usize = 30;
const MAX_GOAL_CHARS: usize = 2000;
const MAX_PHASE_CHARS: usize = 400;
const MAX_NEXT_STEP_CHARS: usize = 2000;
const MAX_NOTES: usize = 12;
const MAX_NOTE_CHARS: usize = 600;
const MAX_PLAN_STEPS: usize = 12;
const MAX_PLAN_STEP_CHARS: usize = 240;
pub const MAX_RECOVERY_ATTEMPTS: u8 = 2;

static AUDIT_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
// ponytail: one service process; use a file lock if multiple writers share a workspace.
static STATE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentStep {
    pub title: String,
    pub status: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RecoveryState {
    pub status: String,
    pub attempts: u8,
    pub last_action: String,
    pub last_target: String,
    pub last_error: String,
    pub last_recovered_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentState {
    pub goal: String,
    pub status: String,
    pub phase: String,
    pub next_step: String,
    pub notes: Vec<String>,
    pub plan: Vec<AgentStep>,
    pub verification_pending: bool,
    pub verification_tool: String,
    pub last_verified_ms: u64,
    pub recovery: RecoveryState,
    pub updated_ms: u64,
}

#[derive(Clone, Debug, Default)]
pub struct AgentUpdate {
    pub goal: Option<String>,
    pub status: Option<String>,
    pub phase: Option<String>,
    pub next_step: Option<String>,
    pub note: Option<String>,
    pub clear_notes: bool,
}

#[derive(Clone, Debug, Default)]
pub struct PlanUpdate {
    pub steps: Option<Vec<String>>,
    pub step_index: Option<usize>,
    pub step_status: Option<String>,
    pub clear: bool,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn bounded(value: String, max_chars: usize, field: &str) -> Result<String, String> {
    if value.chars().count() > max_chars {
        return Err(format!("{field} exceeds {max_chars} characters"));
    }
    Ok(value)
}

fn state_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(STATE_PATH)
}

fn audit_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(AUDIT_PATH)
}

fn valid_task_status(status: &str) -> bool {
    matches!(
        status,
        "queued" | "active" | "waiting" | "verifying" | "blocked" | "done" | "failed"
    )
}

fn valid_step_status(status: &str) -> bool {
    matches!(
        status,
        "pending" | "active" | "verifying" | "blocked" | "done" | "failed"
    )
}

fn save_state(workspace_root: &Path, mut state: AgentState) -> Result<AgentState, String> {
    state.updated_ms = now_ms();
    let path = state_path(workspace_root);
    let parent = path
        .parent()
        .ok_or_else(|| "invalid agent state path".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("create agent state directory: {error}"))?;
    let temp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&state)
        .map_err(|error| format!("serialize agent state: {error}"))?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("open agent state: {error}"))?;
    file.write_all(&bytes)
        .map_err(|error| format!("write agent state: {error}"))?;
    drop(file);
    fs::rename(&temp, &path).map_err(|error| format!("replace agent state: {error}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("protect agent state: {error}"))?;
    }

    Ok(state)
}

pub fn load_state(workspace_root: &Path) -> Result<AgentState, String> {
    let path = state_path(workspace_root);
    if !path.exists() {
        return Ok(AgentState::default());
    }
    let text = fs::read_to_string(&path).map_err(|error| format!("read agent state: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("parse agent state: {error}"))
}

pub fn update_state(workspace_root: &Path, update: AgentUpdate) -> Result<AgentState, String> {
    let _guard = STATE_LOCK
        .lock()
        .map_err(|_| "agent state lock is poisoned".to_string())?;
    let mut state = load_state(workspace_root)?;

    if let Some(goal) = update.goal {
        let goal = bounded(goal, MAX_GOAL_CHARS, "goal")?;
        if !state.goal.is_empty() && state.goal != goal {
            state.plan.clear();
            state.verification_pending = false;
            state.verification_tool.clear();
            state.recovery = RecoveryState::default();
        }
        state.goal = goal;
    }
    if let Some(status) = update.status {
        if !valid_task_status(&status) {
            return Err(
                "status must be one of: queued, active, waiting, verifying, blocked, done, failed"
                    .into(),
            );
        }
        if status == "done" && state.verification_pending {
            return Err(format!(
                "verification is still required after tool '{}'; set status=verifying, perform a successful verification action, then mark done",
                state.verification_tool
            ));
        }
        state.status = status;
    }
    if let Some(phase) = update.phase {
        state.phase = bounded(phase, MAX_PHASE_CHARS, "phase")?;
    }
    if let Some(next_step) = update.next_step {
        state.next_step = bounded(next_step, MAX_NEXT_STEP_CHARS, "next_step")?;
    }
    if update.clear_notes {
        state.notes.clear();
    }
    if let Some(note) = update.note {
        state.notes.push(bounded(note, MAX_NOTE_CHARS, "note")?);
        if state.notes.len() > MAX_NOTES {
            let drop_count = state.notes.len() - MAX_NOTES;
            state.notes.drain(0..drop_count);
        }
    }

    save_state(workspace_root, state)
}

pub fn update_plan(workspace_root: &Path, update: PlanUpdate) -> Result<AgentState, String> {
    let _guard = STATE_LOCK
        .lock()
        .map_err(|_| "agent state lock is poisoned".to_string())?;
    let mut state = load_state(workspace_root)?;

    if update.clear {
        state.plan.clear();
    }
    if let Some(steps) = update.steps {
        if steps.len() > MAX_PLAN_STEPS {
            return Err(format!("plan exceeds {MAX_PLAN_STEPS} steps"));
        }
        state.plan = steps
            .into_iter()
            .map(|title| {
                bounded(title, MAX_PLAN_STEP_CHARS, "plan step").map(|title| AgentStep {
                    title,
                    status: "pending".to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
    }

    match (update.step_index, update.step_status) {
        (Some(index), Some(status)) => {
            if !valid_step_status(&status) {
                return Err(
                    "step_status must be one of: pending, active, verifying, blocked, done, failed"
                        .into(),
                );
            }
            if status == "done" && state.verification_pending {
                return Err(format!(
                    "verification is still required after tool '{}'; verify before marking the plan step done",
                    state.verification_tool
                ));
            }
            let step = state
                .plan
                .get_mut(index)
                .ok_or_else(|| format!("step_index {index} is outside the current plan"))?;
            step.status = status;
        }
        (None, None) => {}
        _ => return Err("step_index and step_status must be provided together".into()),
    }

    if !update.clear && state.plan.is_empty() && update.step_index.is_none() {
        return Err("agent_plan requires steps, a step update, or clear=true".into());
    }

    save_state(workspace_root, state)
}

pub fn note_tool_outcome(
    workspace_root: &Path,
    tool: &str,
    verify_policy: &str,
    ok: bool,
    failure_reason: Option<&str>,
) -> Result<(), String> {
    let _guard = STATE_LOCK
        .lock()
        .map_err(|_| "agent state lock is poisoned".to_string())?;
    let mut state = load_state(workspace_root)?;
    if state.goal.is_empty() || matches!(state.status.as_str(), "done" | "failed") {
        return Ok(());
    }

    if !ok {
        if !matches!(
            tool,
            "agent_status" | "agent_checkpoint" | "agent_plan" | "agent_recover"
        ) {
            state.recovery.status = "needed".to_string();
            state.recovery.last_target = tool.to_string();
            state.recovery.last_error = failure_reason
                .unwrap_or("tool_failed")
                .trim()
                .chars()
                .take(MAX_NOTE_CHARS)
                .collect();
            save_state(workspace_root, state)?;
        }
        return Ok(());
    }

    if verify_policy == "observable_state" {
        state.recovery = RecoveryState::default();
        state.verification_pending = true;
        state.verification_tool = tool.to_string();
        save_state(workspace_root, state)?;
        return Ok(());
    }

    if state.status == "verifying"
        && state.verification_pending
        && matches!(tool, "run_checks" | "parse_checks")
    {
        state.verification_pending = false;
        state.verification_tool.clear();
        state.last_verified_ms = now_ms();
        save_state(workspace_root, state)?;
    }

    Ok(())
}

pub fn start_recovery(
    workspace_root: &Path,
    action: &str,
    target: &str,
) -> Result<AgentState, String> {
    let _guard = STATE_LOCK
        .lock()
        .map_err(|_| "agent state lock is poisoned".to_string())?;
    let mut state = load_state(workspace_root)?;
    if state.goal.is_empty() {
        return Err("no active agent task to recover".into());
    }
    if matches!(state.status.as_str(), "done" | "failed") {
        return Err(format!(
            "task status '{}' cannot be recovered",
            state.status
        ));
    }
    if state.recovery.attempts >= MAX_RECOVERY_ATTEMPTS {
        return Err(format!(
            "recovery attempt limit reached ({MAX_RECOVERY_ATTEMPTS}); block and escalate instead"
        ));
    }
    if state.recovery.last_action == action && state.recovery.last_target == target {
        return Err("the same recovery action may only be attempted once".into());
    }

    state.recovery.attempts += 1;
    state.recovery.status = "recovering".to_string();
    state.recovery.last_action = action.to_string();
    state.recovery.last_target = target.to_string();
    state.recovery.last_error.clear();
    save_state(workspace_root, state)
}

pub fn finish_recovery(
    workspace_root: &Path,
    success: bool,
    clear_verification: bool,
    resume_task: bool,
    error_code: Option<&str>,
) -> Result<AgentState, String> {
    let _guard = STATE_LOCK
        .lock()
        .map_err(|_| "agent state lock is poisoned".to_string())?;
    let mut state = load_state(workspace_root)?;
    if success {
        state.recovery.status = "recovered".to_string();
        state.recovery.last_error.clear();
        state.recovery.last_recovered_ms = now_ms();
        if clear_verification {
            state.verification_pending = false;
            state.verification_tool.clear();
            state.last_verified_ms = now_ms();
        }
        if resume_task && !matches!(state.status.as_str(), "done" | "failed") {
            state.status = "active".to_string();
        }
    } else {
        state.recovery.status = "blocked".to_string();
        state.recovery.last_error = error_code.unwrap_or("recovery_failed").to_string();
        if !matches!(state.status.as_str(), "done" | "failed") {
            state.status = "blocked".to_string();
        }
    }
    save_state(workspace_root, state)
}

pub fn block_recovery(workspace_root: &Path, error_code: &str) -> Result<AgentState, String> {
    let _guard = STATE_LOCK
        .lock()
        .map_err(|_| "agent state lock is poisoned".to_string())?;
    let mut state = load_state(workspace_root)?;
    state.recovery.status = "blocked".to_string();
    state.recovery.last_error = bounded(error_code.to_string(), 120, "recovery error")?;
    if !matches!(state.status.as_str(), "done" | "failed") {
        state.status = "blocked".to_string();
    }
    save_state(workspace_root, state)
}

pub fn record_tool_call(
    workspace_root: &Path,
    tool: &str,
    ok: bool,
    duration_ms: u64,
) -> Result<(), String> {
    let state_file = state_path(workspace_root);
    if !state_file.exists() {
        return Ok(());
    }
    let state = load_state(workspace_root)?;
    if state.goal.is_empty() || matches!(state.status.as_str(), "done" | "failed") {
        return Ok(());
    }

    let _guard = AUDIT_LOCK
        .lock()
        .map_err(|_| "agent audit lock is poisoned".to_string())?;

    let path = audit_path(workspace_root);
    let parent = path
        .parent()
        .ok_or_else(|| "invalid agent audit path".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("create audit directory: {error}"))?;

    if fs::metadata(&path)
        .map(|metadata| metadata.len() > MAX_AUDIT_BYTES)
        .unwrap_or(false)
    {
        let text = fs::read_to_string(&path).unwrap_or_default();
        let lines = text.lines().collect::<Vec<_>>();
        let keep_from = lines.len().saturating_sub(KEEP_AUDIT_LINES);
        let mut kept = lines[keep_from..].join("\n");
        if !kept.is_empty() {
            kept.push('\n');
        }
        fs::write(&path, kept).map_err(|error| format!("rotate agent audit: {error}"))?;
    }

    let entry = json!({
        "finishedMs": now_ms(),
        "tool": tool,
        "ok": ok,
        "durationMs": duration_ms,
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("open agent audit: {error}"))?;
    writeln!(file, "{entry}").map_err(|error| format!("append agent audit: {error}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("protect agent audit: {error}"))?;
    }

    Ok(())
}

pub fn recent_audit(workspace_root: &Path) -> Vec<Value> {
    let path = audit_path(workspace_root);
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut entries = text
        .lines()
        .rev()
        .take(RECENT_AUDIT_LINES)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    entries.reverse();
    entries
}

pub fn audit_health(workspace_root: &Path) -> Value {
    let entries = recent_audit(workspace_root);
    let failures = entries
        .iter()
        .filter(|entry| entry.get("ok").and_then(Value::as_bool) == Some(false))
        .count();
    let consecutive_failures = entries
        .iter()
        .rev()
        .take_while(|entry| entry.get("ok").and_then(Value::as_bool) == Some(false))
        .count();
    let total_duration_ms = entries
        .iter()
        .filter_map(|entry| entry.get("durationMs").and_then(Value::as_u64))
        .sum::<u64>();
    let average_duration_ms = if entries.is_empty() {
        0
    } else {
        total_duration_ms / entries.len() as u64
    };
    let status = if entries.is_empty() {
        "idle"
    } else if consecutive_failures >= 3 || failures * 2 > entries.len() {
        "unhealthy"
    } else if failures > 0 {
        "degraded"
    } else {
        "healthy"
    };

    json!({
        "status": status,
        "recentCalls": entries.len(),
        "failures": failures,
        "consecutiveFailures": consecutive_failures,
        "averageDurationMs": average_duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "catdesk-agent-runtime-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn concurrent_updates_preserve_both_fields() {
        let root = test_root("concurrent");
        update_state(
            &root,
            AgentUpdate {
                goal: Some("concurrent task".into()),
                status: Some("active".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap();
        for round in 0..16 {
            let barrier = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    barrier.wait();
                    update_state(
                        &root,
                        AgentUpdate {
                            phase: Some(format!("phase-{round}")),
                            ..AgentUpdate::default()
                        },
                    )
                    .unwrap();
                });
                scope.spawn(|| {
                    barrier.wait();
                    update_plan(
                        &root,
                        PlanUpdate {
                            steps: Some(vec![format!("step-{round}")]),
                            ..PlanUpdate::default()
                        },
                    )
                    .unwrap();
                });
            });
            let state = load_state(&root).unwrap();
            assert_eq!(state.phase, format!("phase-{round}"));
            assert_eq!(state.plan[0].title, format!("step-{round}"));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verification_requires_a_successful_check_verdict() {
        let root = test_root("check-verdict");
        update_state(
            &root,
            AgentUpdate {
                goal: Some("verify a change".into()),
                status: Some("verifying".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap();
        note_tool_outcome(&root, "write", "observable_state", true, None).unwrap();
        for tool in [
            "read",
            "search",
            "checkpoint_list",
            "git_log",
            "start_command",
            "poll_command",
            "cancel_command",
            "run_command",
        ] {
            note_tool_outcome(&root, tool, "result", true, None).unwrap();
            assert!(
                load_state(&root).unwrap().verification_pending,
                "{tool} cleared verification"
            );
        }
        note_tool_outcome(&root, "run_checks", "result", false, Some("tests failed")).unwrap();
        assert!(load_state(&root).unwrap().verification_pending);
        note_tool_outcome(&root, "parse_checks", "none", true, None).unwrap();
        assert!(!load_state(&root).unwrap().verification_pending);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_round_trip_is_bounded_and_incremental() {
        let root = test_root("state");
        let first = update_state(
            &root,
            AgentUpdate {
                goal: Some("ship the agent runtime".into()),
                status: Some("active".into()),
                phase: Some("implementation".into()),
                next_step: Some("run tests".into()),
                note: Some("keep the diff small".into()),
                clear_notes: false,
            },
        )
        .unwrap();
        assert_eq!(first.notes, vec!["keep the diff small"]);

        let second = update_state(
            &root,
            AgentUpdate {
                status: Some("done".into()),
                note: Some("verified".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap();
        assert_eq!(second.goal, "ship the agent runtime");
        assert_eq!(second.status, "done");
        assert_eq!(second.notes.len(), 2);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn audit_is_lazy_without_an_active_agent_task() {
        let root = test_root("audit-lazy");
        record_tool_call(&root, "create_handoff", true, 4).unwrap();
        assert!(!root.join(".catdesk").exists());
        assert!(recent_audit(&root).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn audit_contains_only_bounded_metadata() {
        let root = test_root("audit");
        update_state(
            &root,
            AgentUpdate {
                goal: Some("audit active task".into()),
                status: Some("active".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap();
        record_tool_call(&root, "read", true, 12).unwrap();
        record_tool_call(&root, "write", false, 34).unwrap();

        let entries = recent_audit(&root);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["tool"], "read");
        assert_eq!(entries[1]["ok"], false);
        assert!(entries[0].get("arguments").is_none());

        let health = audit_health(&root);
        assert_eq!(health["status"], "degraded");
        assert_eq!(health["recentCalls"], 2);
        assert_eq!(health["failures"], 1);

        record_tool_call(&root, "write", false, 35).unwrap();
        record_tool_call(&root, "write", false, 36).unwrap();
        assert_eq!(audit_health(&root)["status"], "unhealthy");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_task_state_is_rejected_without_overwriting_existing_state() {
        let root = test_root("invalid-status");
        update_state(
            &root,
            AgentUpdate {
                goal: Some("safe task".into()),
                status: Some("active".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap();

        let error = update_state(
            &root,
            AgentUpdate {
                status: Some("pwned".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("status must be one of"));

        let state = load_state(&root).unwrap();
        assert_eq!(state.goal, "safe task");
        assert_eq!(state.status, "active");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_v2_state_loads_with_v3_defaults() {
        let root = test_root("legacy");
        let path = state_path(&root);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"goal":"legacy","status":"active","phase":"","next_step":"","notes":[],"updated_ms":1}"#,
        )
        .unwrap();

        let state = load_state(&root).unwrap();
        assert_eq!(state.goal, "legacy");
        assert!(state.plan.is_empty());
        assert!(!state.verification_pending);
        assert_eq!(state.last_verified_ms, 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plan_and_verification_debt_enforce_a_real_verify_step() {
        let root = test_root("verify-gate");
        update_state(
            &root,
            AgentUpdate {
                goal: Some("change and verify".into()),
                status: Some("active".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap();
        update_plan(
            &root,
            PlanUpdate {
                steps: Some(vec!["make change".into(), "verify result".into()]),
                ..PlanUpdate::default()
            },
        )
        .unwrap();
        update_plan(
            &root,
            PlanUpdate {
                step_index: Some(0),
                step_status: Some("active".into()),
                ..PlanUpdate::default()
            },
        )
        .unwrap();

        note_tool_outcome(&root, "write", "observable_state", true, None).unwrap();
        assert!(load_state(&root).unwrap().verification_pending);

        let step_error = update_plan(
            &root,
            PlanUpdate {
                step_index: Some(0),
                step_status: Some("done".into()),
                ..PlanUpdate::default()
            },
        )
        .unwrap_err();
        assert!(step_error.contains("verification is still required"));

        let task_error = update_state(
            &root,
            AgentUpdate {
                status: Some("done".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap_err();
        assert!(task_error.contains("verification is still required"));

        update_state(
            &root,
            AgentUpdate {
                status: Some("verifying".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap();
        note_tool_outcome(&root, "run_checks", "result", true, None).unwrap();

        let verified = load_state(&root).unwrap();
        assert!(!verified.verification_pending);
        assert!(verified.last_verified_ms > 0);

        update_plan(
            &root,
            PlanUpdate {
                step_index: Some(0),
                step_status: Some("done".into()),
                ..PlanUpdate::default()
            },
        )
        .unwrap();
        let done = update_state(
            &root,
            AgentUpdate {
                status: Some("done".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap();
        assert_eq!(done.status, "done");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_is_bounded_and_never_repeats_the_same_action() {
        let root = test_root("recovery-bounds");
        update_state(
            &root,
            AgentUpdate {
                goal: Some("recover safely".into()),
                status: Some("blocked".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap();

        let first = start_recovery(&root, "resume", "task").unwrap();
        assert_eq!(first.recovery.attempts, 1);
        assert_eq!(first.recovery.status, "recovering");
        let recovered = finish_recovery(&root, true, false, true, None).unwrap();
        assert_eq!(recovered.status, "active");
        assert_eq!(recovered.recovery.status, "recovered");

        let repeated = start_recovery(&root, "resume", "task").unwrap_err();
        assert!(repeated.contains("same recovery action"));

        start_recovery(&root, "rollback_latest", "write").unwrap();
        let blocked =
            finish_recovery(&root, false, false, false, Some("restore_incomplete")).unwrap();
        assert_eq!(blocked.status, "blocked");
        assert_eq!(blocked.recovery.attempts, MAX_RECOVERY_ATTEMPTS);
        assert_eq!(blocked.recovery.last_error, "restore_incomplete");
        let exhausted = start_recovery(&root, "other", "task").unwrap_err();
        assert!(exhausted.contains("attempt limit"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tool_failure_persists_replan_context() {
        let root = test_root("recovery-needed");
        update_state(
            &root,
            AgentUpdate {
                goal: Some("recover a failed call".into()),
                status: Some("active".into()),
                ..AgentUpdate::default()
            },
        )
        .unwrap();

        note_tool_outcome(
            &root,
            "read",
            "none",
            false,
            Some("permission denied while reading durable context"),
        )
        .unwrap();
        let state = load_state(&root).unwrap();
        assert_eq!(state.recovery.status, "needed");
        assert_eq!(state.recovery.last_target, "read");
        assert_eq!(
            state.recovery.last_error,
            "permission denied while reading durable context"
        );

        fs::remove_dir_all(root).unwrap();
    }
}
