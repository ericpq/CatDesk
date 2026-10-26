use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use reqwest::{Client, Method, Url};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const API_BASE: &str = "https://api.github.com";
const TOKEN_RELATIVE_PATH: &str = ".catdesk/secrets/github_token";
const MAX_TEXT_FILE_BYTES: u64 = 256 * 1024;
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

pub async fn read(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let repo = required_str(arguments, "repo")?;
    let action = required_str(arguments, "action")?;
    let (owner, name) = repo_parts(repo)?;
    let token = load_token(Path::new(workspace_root))?;
    let client = github_client()?;

    match action {
        "repo" => {
            let url = repo_url(&owner, &name, &[])?;
            let raw = api_json(&client, &token, Method::GET, url, None).await?;
            Ok(json!({
                "action": action,
                "repo": repo,
                "repository": {
                    "fullName": raw.get("full_name"),
                    "private": raw.get("private"),
                    "defaultBranch": raw.get("default_branch"),
                    "description": raw.get("description"),
                    "htmlUrl": raw.get("html_url"),
                    "updatedAt": raw.get("updated_at"),
                    "permissions": raw.get("permissions"),
                }
            }))
        }
        "contents" => {
            let path = optional_str(arguments, "path").unwrap_or("");
            let mut url = contents_url(&owner, &name, path)?;
            if let Some(reference) = optional_str(arguments, "ref") {
                url.query_pairs_mut().append_pair("ref", reference);
            }
            let raw = api_json(&client, &token, Method::GET, url, None).await?;
            let items = raw.as_array().ok_or_else(|| {
                "GitHub returned a file for action=contents; use action=file for a file path"
                    .to_string()
            })?;
            let entries: Vec<Value> = items
                .iter()
                .map(|item| {
                    json!({
                        "name": item.get("name"),
                        "path": item.get("path"),
                        "type": item.get("type"),
                        "sha": item.get("sha"),
                        "size": item.get("size"),
                        "htmlUrl": item.get("html_url"),
                    })
                })
                .collect();
            Ok(json!({"action": action, "repo": repo, "path": path, "entries": entries}))
        }
        "file" => {
            let path = required_str(arguments, "path")?;
            let mut url = contents_url(&owner, &name, path)?;
            if let Some(reference) = optional_str(arguments, "ref") {
                url.query_pairs_mut().append_pair("ref", reference);
            }
            let raw = api_json(&client, &token, Method::GET, url, None).await?;
            let kind = raw.get("type").and_then(Value::as_str).unwrap_or("");
            if kind != "file" {
                return Err("Requested GitHub path is not a file".to_string());
            }
            let size = raw.get("size").and_then(Value::as_u64).unwrap_or(0);
            if size > MAX_TEXT_FILE_BYTES {
                return Err(format!(
                    "GitHub file is larger than the {} KiB text-read limit",
                    MAX_TEXT_FILE_BYTES / 1024
                ));
            }
            let encoding = raw.get("encoding").and_then(Value::as_str).unwrap_or("");
            if encoding != "base64" {
                return Err("GitHub returned an unsupported file encoding".to_string());
            }
            let encoded = raw
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| "GitHub file response contained no content".to_string())?
                .replace(['\r', '\n'], "");
            let bytes = BASE64
                .decode(encoded.as_bytes())
                .map_err(|_| "GitHub file content was not valid base64".to_string())?;
            let text = String::from_utf8(bytes)
                .map_err(|_| "GitHub file is not UTF-8 text".to_string())?;
            Ok(json!({
                "action": action,
                "repo": repo,
                "path": raw.get("path"),
                "sha": raw.get("sha"),
                "size": size,
                "content": text,
                "htmlUrl": raw.get("html_url"),
            }))
        }
        "branches" => {
            let limit = requested_limit(arguments)?;
            let mut url = repo_url(&owner, &name, &["branches"])?;
            url.query_pairs_mut()
                .append_pair("per_page", &limit.to_string());
            let raw = api_json(&client, &token, Method::GET, url, None).await?;
            let branches: Vec<Value> = raw
                .as_array()
                .ok_or_else(|| "GitHub branch response was not an array".to_string())?
                .iter()
                .map(|item| {
                    json!({
                        "name": item.get("name"),
                        "protected": item.get("protected"),
                        "sha": item.pointer("/commit/sha"),
                    })
                })
                .collect();
            Ok(json!({"action": action, "repo": repo, "branches": branches}))
        }
        "commits" => {
            let limit = requested_limit(arguments)?;
            let mut url = repo_url(&owner, &name, &["commits"])?;
            url.query_pairs_mut()
                .append_pair("per_page", &limit.to_string());
            if let Some(reference) = optional_str(arguments, "ref") {
                url.query_pairs_mut().append_pair("sha", reference);
            }
            let raw = api_json(&client, &token, Method::GET, url, None).await?;
            let commits: Vec<Value> = raw
                .as_array()
                .ok_or_else(|| "GitHub commit response was not an array".to_string())?
                .iter()
                .map(|item| {
                    json!({
                        "sha": item.get("sha"),
                        "message": item.pointer("/commit/message"),
                        "authorName": item.pointer("/commit/author/name"),
                        "authorDate": item.pointer("/commit/author/date"),
                        "htmlUrl": item.get("html_url"),
                    })
                })
                .collect();
            Ok(json!({"action": action, "repo": repo, "commits": commits}))
        }
        _ => Err("action must be one of: repo, contents, file, branches, commits".to_string()),
    }
}

pub async fn write(workspace_root: &str, arguments: &Value) -> Result<Value, String> {
    let repo = required_str(arguments, "repo")?;
    let action = required_str(arguments, "action")?;
    let (owner, name) = repo_parts(repo)?;
    let token = load_token(Path::new(workspace_root))?;
    let client = github_client()?;

    match action {
        "create_branch" => {
            let branch = required_str(arguments, "branch")?;
            let base = required_str(arguments, "base")?;
            validate_ref_name(branch)?;
            validate_ref_name(base)?;

            let base_url = repo_ref_url(&owner, &name, base)?;
            let base_ref = api_json(&client, &token, Method::GET, base_url, None).await?;
            let sha = base_ref
                .pointer("/object/sha")
                .and_then(Value::as_str)
                .ok_or_else(|| "GitHub base branch response contained no commit SHA".to_string())?;

            let refs_url = repo_url(&owner, &name, &["git", "refs"])?;
            let created = api_json(
                &client,
                &token,
                Method::POST,
                refs_url,
                Some(json!({"ref": format!("refs/heads/{branch}"), "sha": sha})),
            )
            .await?;
            Ok(json!({
                "action": action,
                "repo": repo,
                "branch": branch,
                "base": base,
                "sha": created.pointer("/object/sha"),
            }))
        }
        "create_file" | "update_file" => {
            let path = required_str(arguments, "path")?;
            let branch = required_str(arguments, "branch")?;
            let message = required_str(arguments, "message")?;
            let content = required_str(arguments, "content")?;
            validate_ref_name(branch)?;
            let url = contents_url(&owner, &name, path)?;
            let mut body = json!({
                "message": message,
                "content": BASE64.encode(content.as_bytes()),
                "branch": branch,
            });
            if action == "update_file" {
                let sha = required_str(arguments, "sha")?;
                body.as_object_mut()
                    .expect("JSON object")
                    .insert("sha".to_string(), json!(sha));
            }
            let raw = api_json(&client, &token, Method::PUT, url, Some(body)).await?;
            Ok(json!({
                "action": action,
                "repo": repo,
                "path": path,
                "branch": branch,
                "commitSha": raw.pointer("/commit/sha"),
                "contentSha": raw.pointer("/content/sha"),
                "htmlUrl": raw.pointer("/content/html_url"),
            }))
        }
        "create_pr" => {
            let title = required_str(arguments, "title")?;
            let head = required_str(arguments, "head")?;
            let base = required_str(arguments, "base")?;
            validate_ref_name(head)?;
            validate_ref_name(base)?;
            let body = optional_str(arguments, "body").unwrap_or("");
            let url = repo_url(&owner, &name, &["pulls"])?;
            let raw = api_json(
                &client,
                &token,
                Method::POST,
                url,
                Some(json!({"title": title, "head": head, "base": base, "body": body})),
            )
            .await?;
            Ok(json!({
                "action": action,
                "repo": repo,
                "number": raw.get("number"),
                "title": raw.get("title"),
                "state": raw.get("state"),
                "head": raw.pointer("/head/ref"),
                "base": raw.pointer("/base/ref"),
                "htmlUrl": raw.get("html_url"),
            }))
        }
        _ => Err(
            "action must be one of: create_branch, create_file, update_file, create_pr".to_string(),
        ),
    }
}

fn github_client() -> Result<Client, String> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("CatDesk/0.9 GitHub integration")
        .build()
        .map_err(|_| "Failed to initialize GitHub HTTP client".to_string())
}

fn load_token(workspace_root: &Path) -> Result<String, String> {
    let secret_path = workspace_root.join(TOKEN_RELATIVE_PATH);
    if let Ok(raw) = std::fs::read_to_string(&secret_path) {
        let token = raw.trim();
        if !token.is_empty() {
            return Ok(token.to_string());
        }
    }

    for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(raw) = std::env::var(name) {
            let token = raw.trim();
            if !token.is_empty() {
                return Ok(token.to_string());
            }
        }
    }

    if let Ok(output) = Command::new("gh").args(["auth", "token"]).output()
        && output.status.success()
    {
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    Err(
        "No GitHub credential is available to CatDesk. Configure gh auth for the CatDesk service user or store a token in .catdesk/secrets/github_token."
            .to_string(),
    )
}

async fn api_json(
    client: &Client,
    token: &str,
    method: Method,
    url: Url,
    body: Option<Value>,
) -> Result<Value, String> {
    let mut request = client
        .request(method, url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "GitHub request failed before a response was received".to_string())?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "request failed".to_string());
        return Err(format!(
            "GitHub returned HTTP {}: {}",
            status.as_u16(),
            detail
        ));
    }
    serde_json::from_str(&text).map_err(|_| "GitHub response was not valid JSON".to_string())
}

fn repo_parts(repo: &str) -> Result<(String, String), String> {
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or("");
    let name = parts.next().unwrap_or("");
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return Err("repo must be in owner/name form".to_string());
    }
    if !safe_repo_component(owner) || !safe_repo_component(name) {
        return Err("repo contains unsupported characters".to_string());
    }
    Ok((owner.to_string(), name.to_string()))
}

fn safe_repo_component(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn validate_ref_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains("..")
        || value.contains('~')
        || value.contains('^')
        || value.contains(':')
        || value.contains('?')
        || value.contains('*')
        || value.contains('[')
        || value.contains('\\')
        || value.contains(char::is_whitespace)
    {
        return Err("branch/base contains unsupported Git ref characters".to_string());
    }
    Ok(())
}

fn repo_url(owner: &str, name: &str, tail: &[&str]) -> Result<Url, String> {
    let mut url = Url::parse(API_BASE).map_err(|_| "Invalid GitHub API base URL".to_string())?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Could not build GitHub API URL".to_string())?;
        segments.extend(["repos", owner, name]);
        segments.extend(tail.iter().copied());
    }
    Ok(url)
}

fn contents_url(owner: &str, name: &str, path: &str) -> Result<Url, String> {
    let mut url = repo_url(owner, name, &["contents"])?;
    if !path.is_empty() {
        let parts = safe_path_parts(path)?;
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Could not build GitHub contents URL".to_string())?;
        segments.extend(parts);
    }
    Ok(url)
}

fn repo_ref_url(owner: &str, name: &str, reference: &str) -> Result<Url, String> {
    let mut url = repo_url(owner, name, &["git", "ref", "heads"])?;
    let parts = safe_path_parts(reference)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Could not build GitHub ref URL".to_string())?;
        segments.extend(parts);
    }
    Ok(url)
}

fn safe_path_parts(path: &str) -> Result<Vec<&str>, String> {
    if path.starts_with('/') || path.ends_with('/') {
        return Err(
            "path must be repository-relative without leading or trailing slash".to_string(),
        );
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err("path contains an invalid segment".to_string());
    }
    Ok(parts)
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

fn requested_limit(arguments: &Value) -> Result<usize, String> {
    match arguments.get("limit") {
        None => Ok(DEFAULT_LIMIT),
        Some(value) => {
            let limit = value
                .as_u64()
                .ok_or_else(|| "limit must be an integer".to_string())?
                as usize;
            if !(1..=MAX_LIMIT).contains(&limit) {
                return Err(format!("limit must be between 1 and {MAX_LIMIT}"));
            }
            Ok(limit)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_repo_and_paths() {
        assert!(repo_parts("AndrewYeah/hunan-museum").is_ok());
        assert!(repo_parts("bad").is_err());
        assert!(safe_path_parts("src/main.rs").is_ok());
        assert!(safe_path_parts("../secret").is_err());
        assert!(validate_ref_name("feature/museum-home").is_ok());
        assert!(validate_ref_name("bad..ref").is_err());
    }

    #[tokio::test]
    #[ignore]
    async fn live_repo_access() {
        let repo = std::env::var("CATDESK_GITHUB_TEST_REPO").expect("set CATDESK_GITHUB_TEST_REPO");
        let root = std::env::var("CATDESK_GITHUB_WORKSPACE").unwrap_or_else(|_| {
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .to_string()
        });
        let result = read(&root, &json!({"action": "repo", "repo": repo}))
            .await
            .expect("GitHub repository should be readable");
        eprintln!(
            "repo={} private={} default_branch={}",
            result
                .pointer("/repository/fullName")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            result
                .pointer("/repository/private")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            result
                .pointer("/repository/defaultBranch")
                .and_then(Value::as_str)
                .unwrap_or("?")
        );
    }
}
