use crate::command;
use base64::Engine as _;
use reqwest::Client;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const API_BASE: &str = "https://qiurui.172363.xyz";
const TOKEN_RELATIVE_PATH: &str = ".catdesk/secrets/qiurui_game_token";
const MAX_IMAGE_BYTES: usize = 15 * 1024 * 1024;

pub struct TaskParams {
    pub action: String,
    pub task_id: Option<String>,
    pub status: Option<String>,
    pub image_path: Option<String>,
    pub image_url: Option<String>,
    pub result: Option<String>,
    pub changes: Option<Value>,
    pub error: Option<String>,
}

pub async fn run(workspace_root: &str, params: TaskParams) -> Result<Value, String> {
    let token = load_token(Path::new(workspace_root))?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| "Failed to initialize Qiurui HTTP client".to_string())?;

    match params.action.as_str() {
        "list" => {
            let mut req = client
                .get(format!("{API_BASE}/api/agent/tasks"))
                .header("X-Qiurui-Agent-Token", &token);
            if let Some(status) = params.status.as_deref().filter(|v| !v.is_empty()) {
                req = req.query(&[("status", status)]);
            }
            api_json(req, "Qiurui task list").await
        }
        "claim" => {
            let task_id = required_task_id(&params)?;
            api_json(
                client
                    .post(format!("{API_BASE}/api/agent/tasks/{task_id}/claim"))
                    .header("X-Qiurui-Agent-Token", &token),
                "Qiurui task claim",
            )
            .await
        }
        "fail" => {
            let task_id = required_task_id(&params)?;
            let error = params.error.as_deref().unwrap_or("generation failed");
            api_json(
                client
                    .post(format!("{API_BASE}/api/agent/tasks/{task_id}/fail"))
                    .header("X-Qiurui-Agent-Token", &token)
                    .json(&json!({"error": error})),
                "Qiurui task fail",
            )
            .await
        }
        "complete" => {
            let task_id = required_task_id(&params)?;
            let payload =
                if let Some(result) = params.result.as_deref().filter(|v| !v.trim().is_empty()) {
                    let mut payload = json!({"result": result});
                    if let Some(changes) = params.changes.clone() {
                        payload["changes"] = changes;
                    }
                    payload
                } else if let Some(path) = params.image_path.as_deref().filter(|v| !v.is_empty()) {
                    let path = command::resolve_workspace_path(workspace_root, Some(path))?;
                    let bytes = std::fs::read(&path)
                        .map_err(|_| format!("Could not read image {}", path.display()))?;
                    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
                        return Err("Image is empty or larger than 15 MB".to_string());
                    }
                    let content_type = image_content_type(
                        path.file_name()
                            .and_then(|v| v.to_str())
                            .ok_or_else(|| "Image filename is not valid UTF-8".to_string())?,
                    )?;
                    json!({
                        "image_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
                        "content_type": content_type,
                    })
                } else if let Some(url) = params.image_url.as_deref().filter(|v| !v.is_empty()) {
                    if !url.starts_with("https://") {
                        return Err("image_url must use https".to_string());
                    }
                    json!({"image_url": url})
                } else {
                    return Err("complete requires result, image_path, or image_url".to_string());
                };
            api_json(
                client
                    .post(format!("{API_BASE}/api/agent/tasks/{task_id}/complete"))
                    .header("X-Qiurui-Agent-Token", &token)
                    .json(&payload),
                "Qiurui task complete",
            )
            .await
        }
        "apply" => {
            let task_id = required_task_id(&params)?;
            api_json(
                client
                    .post(format!("{API_BASE}/api/agent/tasks/{task_id}/apply"))
                    .header("X-Qiurui-Agent-Token", &token)
                    .json(&json!({})),
                "Qiurui task apply",
            )
            .await
        }
        _ => Err("action must be list, claim, complete, apply, or fail".to_string()),
    }
}

fn required_task_id(params: &TaskParams) -> Result<&str, String> {
    params
        .task_id
        .as_deref()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "task_id is required for this action".to_string())
}

fn load_token(workspace_root: &Path) -> Result<String, String> {
    let path = workspace_root.join(TOKEN_RELATIVE_PATH);
    let token = std::fs::read_to_string(&path)
        .map_err(|_| format!("Missing Qiurui Game token file at {}", path.display()))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("Qiurui Game token file is empty".to_string());
    }
    Ok(token)
}

fn image_content_type(filename: &str) -> Result<&'static str, String> {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".png") {
        Ok("image/png")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Ok("image/jpeg")
    } else if lower.ends_with(".webp") {
        Ok("image/webp")
    } else {
        Err("Qiurui Game assets support PNG, JPG, and WEBP".to_string())
    }
}

async fn api_json(request: reqwest::RequestBuilder, label: &str) -> Result<Value, String> {
    let response = request
        .send()
        .await
        .map_err(|_| format!("{label} request failed before a response was received"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "{label} returned HTTP {}: {}",
            status,
            text.replace(['\r', '\n'], " ")
                .chars()
                .take(500)
                .collect::<String>()
        ));
    }
    serde_json::from_str(&text).map_err(|_| format!("{label} returned non-JSON"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_image_types() {
        assert_eq!(image_content_type("a.png").unwrap(), "image/png");
        assert_eq!(image_content_type("a.JPEG").unwrap(), "image/jpeg");
        assert!(image_content_type("a.gif").is_err());
    }
}
