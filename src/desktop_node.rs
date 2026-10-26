use serde_json::{Value, json};
use std::path::Path;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

const SCRIPT_RELATIVE_PATH: &str = "catdesk-desktop-node/desktopctl.py";
const DEFAULT_WAIT_SECONDS: u64 = 20;
const MAX_WAIT_SECONDS: u64 = 300;
const MAX_SCRIPT_BYTES: usize = 64 * 1024;
const MAX_TEXT_BYTES: usize = 16 * 1024;

pub async fn run(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let action = required_str(arguments, "action")?;
    let script = Path::new(workspace_root).join(SCRIPT_RELATIVE_PATH);
    if !script.is_file() {
        return Err(format!(
            "Desktop Node controller is missing: {SCRIPT_RELATIVE_PATH}"
        ));
    }

    let (args, wait_seconds) = command_args(action, arguments)?;
    let mut command = Command::new("python3");
    command.current_dir(workspace_root).arg(&script).args(&args);

    let output = timeout(
        Duration::from_secs(wait_seconds.saturating_add(10)),
        command.output(),
    )
    .await
    .map_err(|_| format!("Desktop Node action timed out after {wait_seconds}s"))?
    .map_err(|_| "Failed to start the Desktop Node controller".to_string())?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let result: Value = serde_json::from_str(stdout.trim()).map_err(|_| {
        let detail = if stderr.trim().is_empty() {
            "controller returned no valid JSON".to_string()
        } else {
            stderr.trim().chars().take(1000).collect()
        };
        format!("Desktop Node controller failed: {detail}")
    })?;

    if !output.status.success() {
        let detail = result
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Desktop Node action failed");
        return Err(detail.to_string());
    }

    Ok(json!({
        "action": action,
        "result": result,
    }))
}

fn command_args(action: &str, arguments: &Value) -> Result<(Vec<String>, u64), String> {
    let wait = optional_u64(arguments, "wait")?
        .unwrap_or(DEFAULT_WAIT_SECONDS)
        .clamp(1, MAX_WAIT_SECONDS);

    let mut args = match action {
        "status" => vec!["status".to_string()],
        "ping" => vec!["ping".to_string()],
        "system_info" => vec!["system-info".to_string()],
        "processes" => vec!["processes".to_string()],
        "windows" => vec!["windows".to_string()],
        "screenshot" => vec!["screenshot".to_string()],
        "powershell" => {
            let script = required_str(arguments, "script")?;
            if script.len() > MAX_SCRIPT_BYTES {
                return Err(format!("script exceeds the {MAX_SCRIPT_BYTES}-byte limit"));
            }
            vec![
                "powershell".to_string(),
                "--script".to_string(),
                script.to_string(),
                "--timeout".to_string(),
                optional_u64(arguments, "timeout")?
                    .unwrap_or(60)
                    .clamp(1, MAX_WAIT_SECONDS)
                    .to_string(),
            ]
        }
        "mouse_move" => vec![
            "mouse-move".to_string(),
            "--x".to_string(),
            required_i64(arguments, "x")?.to_string(),
            "--y".to_string(),
            required_i64(arguments, "y")?.to_string(),
        ],
        "mouse_click" => {
            let button = optional_str(arguments, "button").unwrap_or("left");
            if !matches!(button, "left" | "right" | "middle") {
                return Err("button must be left, right, or middle".to_string());
            }
            let mut values = vec![
                "mouse-click".to_string(),
                "--button".to_string(),
                button.to_string(),
                "--clicks".to_string(),
                optional_u64(arguments, "clicks")?
                    .unwrap_or(1)
                    .clamp(1, 3)
                    .to_string(),
            ];
            match (arguments.get("x"), arguments.get("y")) {
                (None, None) => {}
                (Some(_), Some(_)) => {
                    values.extend([
                        "--x".to_string(),
                        required_i64(arguments, "x")?.to_string(),
                        "--y".to_string(),
                        required_i64(arguments, "y")?.to_string(),
                    ]);
                }
                _ => return Err("x and y must be provided together".to_string()),
            }
            values
        }
        "mouse_scroll" => vec![
            "mouse-scroll".to_string(),
            "--delta".to_string(),
            required_i64(arguments, "delta")?.to_string(),
        ],
        "mouse_drag" => vec![
            "mouse-drag".to_string(),
            "--x1".to_string(),
            required_i64(arguments, "x1")?.to_string(),
            "--y1".to_string(),
            required_i64(arguments, "y1")?.to_string(),
            "--x2".to_string(),
            required_i64(arguments, "x2")?.to_string(),
            "--y2".to_string(),
            required_i64(arguments, "y2")?.to_string(),
            "--duration-ms".to_string(),
            optional_u64(arguments, "duration_ms")?
                .unwrap_or(300)
                .clamp(80, 3000)
                .to_string(),
        ],
        "key" => vec![
            "key".to_string(),
            "--combo".to_string(),
            required_str(arguments, "combo")?.to_string(),
        ],
        "text" => {
            let text = required_str(arguments, "text")?;
            if text.len() > MAX_TEXT_BYTES {
                return Err(format!("text exceeds the {MAX_TEXT_BYTES}-byte limit"));
            }
            vec!["text".to_string(), "--text".to_string(), text.to_string()]
        }
        _ => {
            return Err(
                "action must be one of: status, ping, system_info, processes, windows, screenshot, powershell, mouse_move, mouse_click, mouse_scroll, mouse_drag, key, text"
                    .to_string(),
            );
        }
    };

    if action != "status" && action != "powershell" {
        args.extend(["--wait".to_string(), wait.to_string()]);
    }

    let process_wait = if action == "powershell" {
        optional_u64(arguments, "timeout")?
            .unwrap_or(60)
            .clamp(1, MAX_WAIT_SECONDS)
            .saturating_add(20)
    } else if action == "status" {
        5
    } else {
        wait
    };

    Ok((args, process_wait))
}

fn required_str<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, String> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("Missing required string parameter: {name}"))
}

fn optional_str<'a>(arguments: &'a Value, name: &str) -> Option<&'a str> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn required_i64(arguments: &Value, name: &str) -> Result<i64, String> {
    arguments
        .get(name)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("Missing required integer parameter: {name}"))
}

fn optional_u64(arguments: &Value, name: &str) -> Result<Option<u64>, String> {
    match arguments.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{name} must be a non-negative integer")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_safe_ping_and_powershell_arguments() {
        let (args, wait) = command_args("ping", &json!({"action":"ping","wait":15})).unwrap();
        assert_eq!(args, vec!["ping", "--wait", "15"]);
        assert_eq!(wait, 15);

        let (args, wait) = command_args(
            "powershell",
            &json!({"action":"powershell","script":"Get-Date","timeout":10}),
        )
        .unwrap();
        assert_eq!(
            args,
            vec!["powershell", "--script", "Get-Date", "--timeout", "10"]
        );
        assert_eq!(wait, 30);
    }
}
