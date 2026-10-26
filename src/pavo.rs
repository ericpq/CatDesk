use crate::command;
use regex::Regex;
use reqwest::{
    Client,
    header::{HeaderName, HeaderValue},
};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

const API_BASE: &str = "https://api.pavo-ai.cn/api";
const TOKEN_RELATIVE_PATH: &str = ".catdesk/secrets/pavo_token";
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const POLL_INTERVAL_SECS: u64 = 3;
const MAX_POLLS: usize = 80;

#[derive(Debug)]
pub struct GenerateParams {
    pub conversation_id: String,
    pub image_path: String,
    pub prompt: String,
    pub model: String,
    pub ratio: String,
    pub duration: u64,
    pub resolution: String,
    pub sound: String,
}

pub async fn image_to_video(workspace_root: &str, params: GenerateParams) -> Result<Value, String> {
    let token = load_token(Path::new(workspace_root))?;
    let image_path = command::resolve_workspace_path(workspace_root, Some(&params.image_path))?;
    let image = std::fs::read(&image_path)
        .map_err(|_| format!("Could not read image {}", image_path.display()))?;
    if image.is_empty() {
        return Err("Image file is empty".to_string());
    }
    if image.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "Image is larger than {} MB",
            MAX_IMAGE_BYTES / 1024 / 1024
        ));
    }
    let filename = image_path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| "Image filename is not valid UTF-8".to_string())?;
    let content_type = image_content_type(filename)?;

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|_| "Failed to initialize PAVO HTTP client".to_string())?;

    let before = fetch_history_urls(&client, &token, &params.conversation_id)
        .await
        .unwrap_or_default();

    let presign = api_json(
        client
            .post(format!("{API_BASE}/v1/file/presigned-url"))
            .bearer_auth(&token)
            .json(&json!({
                "purpose": "chat_attachment",
                "content_type": content_type,
                "filename": filename
            })),
        "PAVO presigned-url",
    )
    .await?;
    ensure_pavo_success(&presign, "presigned-url")?;
    let data = presign
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| "PAVO presigned-url response had no data object".to_string())?;
    let upload_url = required_str(data, "upload_url")?;
    let public_url = required_str(data, "public_url")?.to_string();
    let meta_data = data.get("meta_data").cloned().unwrap_or_else(|| {
        json!({
            "original_filename": filename,
            "content_type": content_type,
            "purpose": "chat_attachment"
        })
    });

    let mut upload = client.put(upload_url).body(image);
    if let Some(headers) = data.get("required_headers").and_then(Value::as_object) {
        for (name, value) in headers {
            let Some(value) = value.as_str() else {
                continue;
            };
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "PAVO returned an invalid upload header name".to_string())?;
            let value = HeaderValue::from_str(value)
                .map_err(|_| "PAVO returned an invalid upload header value".to_string())?;
            upload = upload.header(name, value);
        }
    }
    let upload_response = upload
        .send()
        .await
        .map_err(|_| "PAVO image upload failed before a response was received".to_string())?;
    if !upload_response.status().is_success() {
        return Err(format!(
            "PAVO image upload returned HTTP {}",
            upload_response.status()
        ));
    }

    let inspect = api_json(
        client
            .post(format!("{API_BASE}/v1/file/inspect"))
            .bearer_auth(&token)
            .header("X-User-Language", "zh")
            .json(&json!({"content_type": content_type, "url": public_url})),
        "PAVO file inspect",
    )
    .await?;
    ensure_pavo_success(&inspect, "file inspect")?;

    let creative = serde_json::to_string(&json!([
        {"type": "text", "content": params.prompt}
    ]))
    .map_err(|_| "Failed to encode PAVO creative prompt".to_string())?;
    let payload = json!({
        "conversation_id": params.conversation_id,
        "prompt": params.prompt,
        "mode": "omni_to_video",
        "model": params.model,
        "ratio": params.ratio,
        "duration": params.duration,
        "resolution": params.resolution,
        "sound": params.sound,
        "creative_prompt_json": creative,
        "images": [{"url": public_url, "meta_data": meta_data}]
    });

    let response = client
        .post(format!("{API_BASE}/v1/chat/stream"))
        .bearer_auth(&token)
        .header("X-Platform", "1")
        .header("X-App-Timezone", "Asia/Shanghai")
        .header("X-User-Language", "zh")
        .header("Accept", "text/event-stream")
        .header("Referer", "https://app.pavo-ai.cn/")
        .json(&payload)
        .send()
        .await
        .map_err(|_| {
            "PAVO video generation request failed before a response was received".to_string()
        })?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "PAVO video generation returned HTTP {}: {}",
            status,
            safe_excerpt(&body)
        ));
    }
    let stream_text = response.text().await.unwrap_or_default();
    let mut candidates = mp4_urls_from_text(&stream_text);
    candidates.retain(|url| !before.contains(url));

    let video_url = if let Some(url) = candidates.into_iter().next() {
        url
    } else {
        let mut found = None;
        for _ in 0..MAX_POLLS {
            let urls = fetch_history_urls(&client, &token, &params.conversation_id).await?;
            if let Some(url) = urls.into_iter().find(|url| !before.contains(url)) {
                found = Some(url);
                break;
            }
            tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
        }
        found.ok_or_else(|| {
            format!(
                "PAVO accepted the generation request, but no new MP4 appeared within {} seconds",
                MAX_POLLS as u64 * POLL_INTERVAL_SECS
            )
        })?
    };

    let verify = client
        .head(&video_url)
        .send()
        .await
        .map_err(|_| "Generated PAVO video URL could not be verified".to_string())?;
    if !verify.status().is_success() {
        return Err(format!(
            "Generated PAVO video URL returned HTTP {}",
            verify.status()
        ));
    }

    Ok(json!({
        "success": true,
        "conversationId": params.conversation_id,
        "imageUrl": public_url,
        "videoUrl": video_url,
        "model": params.model,
        "duration": params.duration,
        "verified": true
    }))
}

fn load_token(workspace_root: &Path) -> Result<String, String> {
    let path = workspace_root.join(TOKEN_RELATIVE_PATH);
    let token = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "Missing PAVO token file at {}. Store the bearer token there; it is never accepted as a tool argument.",
            path.display()
        )
    })?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("PAVO token file is empty".to_string());
    }
    Ok(token)
}

fn image_content_type(filename: &str) -> Result<&'static str, String> {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Ok("image/jpeg")
    } else if lower.ends_with(".png") {
        Ok("image/png")
    } else if lower.ends_with(".webp") {
        Ok("image/webp")
    } else {
        Err("PAVO test tool supports JPG, PNG, and WEBP images".to_string())
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
            safe_excerpt(&text)
        ));
    }
    serde_json::from_str(&text)
        .map_err(|_| format!("{label} returned non-JSON: {}", safe_excerpt(&text)))
}

fn ensure_pavo_success(value: &Value, label: &str) -> Result<(), String> {
    let code = value.get("code").and_then(Value::as_str).unwrap_or("");
    if code == "000000" {
        return Ok(());
    }
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    Err(format!("PAVO {label} failed: {code} {message}"))
}

async fn fetch_history_urls(
    client: &Client,
    token: &str,
    conversation_id: &str,
) -> Result<BTreeSet<String>, String> {
    let value = api_json(
        client
            .get(format!(
                "{API_BASE}/v1/chat/conversation/history?conversation_id={conversation_id}"
            ))
            .bearer_auth(token)
            .header("X-Platform", "1")
            .header("X-App-Timezone", "Asia/Shanghai")
            .header("X-User-Language", "zh"),
        "PAVO conversation history",
    )
    .await?;
    let mut urls = BTreeSet::new();
    collect_mp4_urls(&value, &mut urls);
    Ok(urls)
}

fn collect_mp4_urls(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::String(text) => {
            out.extend(mp4_urls_from_text(text));
        }
        Value::Array(items) => {
            for item in items {
                collect_mp4_urls(item, out);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_mp4_urls(value, out);
            }
        }
        _ => {}
    }
}

fn mp4_urls_from_text(text: &str) -> BTreeSet<String> {
    let regex = Regex::new(r#"https?://[^"\\\s]+?\.mp4(?:\?[^"\\\s]*)?"#)
        .expect("static mp4 regex must compile");
    regex
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect()
}

fn required_str<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("PAVO response is missing {key}"))
}

fn safe_excerpt(text: &str) -> String {
    text.replace(['\r', '\n'], " ").chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_image_types() {
        assert_eq!(image_content_type("x.JPG").unwrap(), "image/jpeg");
        assert_eq!(image_content_type("x.png").unwrap(), "image/png");
        assert!(image_content_type("x.gif").is_err());
    }

    #[test]
    fn extracts_mp4_urls_recursively() {
        let value = json!({
            "data": [{"video_url": "https://example.com/a.mp4?x=1"}],
            "nested": "{\"url\":\"https://example.com/b.mp4\"}"
        });
        let mut urls = BTreeSet::new();
        collect_mp4_urls(&value, &mut urls);
        assert!(urls.contains("https://example.com/a.mp4?x=1"));
        assert!(urls.contains("https://example.com/b.mp4"));
    }

    #[tokio::test]
    #[ignore = "live PAVO API smoke test"]
    async fn pavo_live_image_to_video() {
        let conversation_id =
            std::env::var("PAVO_TEST_CONVERSATION_ID").expect("PAVO_TEST_CONVERSATION_ID");
        let image_path = std::env::var("PAVO_TEST_IMAGE").expect("PAVO_TEST_IMAGE");
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace parent")
            .to_str()
            .expect("workspace utf8");
        let result = image_to_video(
            workspace_root,
            GenerateParams {
                conversation_id,
                image_path,
                prompt: "主体自然向前运动，镜头轻微推进，保持画面稳定".to_string(),
                model: "agnes-video-new-flash".to_string(),
                ratio: "9:16".to_string(),
                duration: 4,
                resolution: "hd".to_string(),
                sound: "auto".to_string(),
            },
        )
        .await
        .expect("live PAVO generation");
        assert_eq!(result.get("verified").and_then(Value::as_bool), Some(true));
        println!(
            "PAVO_VIDEO_URL={}",
            result.get("videoUrl").and_then(Value::as_str).unwrap_or("")
        );
    }
}
