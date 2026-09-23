from pathlib import Path

ROOT = Path(".")
UP = "origin/upstream-0.9.3"
BASE = "bfac47b"

def split_conflicts(text: str):
    out, pos, idx = [], 0, 0
    start = "<<<<<<< HEAD\n"
    base = f"||||||| {BASE}\n"
    middle = "=======\n"
    end = f">>>>>>> {UP}\n"
    while True:
        a = text.find(start, pos)
        if a < 0:
            out.append(("text", text[pos:]))
            break
        out.append(("text", text[pos:a]))
        b = text.find(base, a + len(start))
        c = text.find(middle, b + len(base))
        d = text.find(end, c + len(middle))
        if min(b, c, d) < 0:
            raise RuntimeError("malformed diff3 conflict")
        idx += 1
        out.append(("conflict", (
            idx,
            text[a + len(start):b],
            text[b + len(base):c],
            text[c + len(middle):d],
        )))
        pos = d + len(end)
    return out

def resolve(path: str, chooser):
    file = ROOT / path
    text = file.read_text()
    pieces, count = [], 0
    for kind, data in split_conflicts(text):
        if kind == "text":
            pieces.append(data)
        else:
            count += 1
            pieces.append(chooser(*data))
    file.write_text("".join(pieces))
    print(f"resolved {path}: {count} conflict(s)")

def cargo(idx, ours, ancestor, theirs):
    if idx == 1:
        return """# ngrok selects Rustls' AWS-LC provider. Keep Reqwest provider-neutral so
# Rustls sees exactly one process-level provider at runtime; retain the 0.9.3
# HTTP/2 and charset features without enabling Ring through `rustls-tls`.
reqwest = { version = "0.12", default-features = false, features = ["json", "http2", "charset", "rustls-tls-webpki-roots-no-provider"] }
rustls = { version = "0.23", default-features = false, features = ["aws_lc_rs", "std", "tls12"] }
tokio-stream = { version = "0.1", features = ["sync"] }
"""
    if idx == 2:
        return """tree-sitter-rust = "0.24.2"
tree-sitter-python = "0.25.0"
tree-sitter-go = "0.25.0"
tree-sitter-javascript = "0.25.0"
tree-sitter-typescript = "0.23.2"
unicode-width = "0.2"
"""
    raise RuntimeError(f"unexpected Cargo.toml conflict {idx}")

def process_runner(idx, ours, ancestor, theirs):
    if idx == 1:
        return theirs
    raise RuntimeError(f"unexpected process_runner conflict {idx}")

def main(idx, ours, ancestor, theirs):
    if idx == 1:
        return "mod handoff;\nmod iyunzhi;\n"
    if idx == 2:
        return '                                                || line.contains("Monitor")\n                                                || line.contains("MCP 伺服器 URL")\n'
    if idx == 3:
        return """    let full_monitor_url = full_mcp_url
        .as_deref()
        .and_then(|url| url.strip_suffix("/mcp").map(|base| format!("{base}/live")));
    let monitor_url = match (&full_monitor_url, mcp_url_is_revealed) {
        (Some(url), true) => url.clone(),
        (Some(_), false) => MONITOR_URL_MASK.to_string(),
        (None, _) => "--".to_string(),
    };
    let mcp_url_security_status = mcp_url_reveal_remaining.map(|remaining| {
        let seconds = mcp_url_reveal_seconds(remaining);
        if ui_language.is_traditional_chinese() {
            format!("[ 已顯示 {:>2}秒 ]", seconds)
        } else {
            format!("[ EXPOSED {:>2}s ]", seconds)
        }
    });
"""
    if idx == 4:
        return """            status_label(ui_language.text("Monitor", "監控")),
            Span::styled(
                &monitor_url,
                Style::default().fg(if has_url {
                    if mcp_url_is_revealed {
                        palette.info_fg
                    } else {
                        palette.muted_fg
                    }
                } else {
                    palette.muted_fg
                }),
            ),
        ]),
        Line::from(vec![
            status_label(ui_language.text("Workspace", "工作區")),
"""
    raise RuntimeError(f"unexpected main.rs conflict {idx}")

def mcp(idx, ours, ancestor, theirs):
    if idx == 1:
        return "use crate::handoff;\nuse crate::iyunzhi;\n"
    if idx == 2:
        return ours + theirs
    if idx == 3:
        return """    if tool_name == "create_handoff" && !handoff_enabled {
        return tool_error_response(req, "Unknown tool: create_handoff".to_string());
    }

    // Held for the whole call: the widget polls this to show that work is
    // still running. A guard rather than a pair of calls, so an early return
    // cannot leave the strip claiming CatDesk is busy forever.
    let mut activity_guard = activity::begin(&tool_name, activity_detail(req, &tool_name));

    if mode.computer_enabled() && tool_mode.write_tools_enabled() {
        capture_checkpoint_for_request(req, &tool_name, workspace_root);
    }

"""
    if idx == 4:
        return ours + """                    "create_handoff" if handoff_enabled => {
                        handle_create_handoff(req, workspace_root)
                    }
"""
    if idx == 5:
        return '            | "apply_patch"\n            | "create_handoff"\n'
    if idx == 6:
        return '                "create_handoff",\n                "apply_patch",\n'
    if idx == 7:
        return """        assert_eq!(
            names,
            vec![
                "iyunzhi_bi_query",
                "catdesk_instruction",
                "read",
                "search",
                "outline",
                "find_symbol",
                "read_symbol",
                "git_status",
                "git_diff",
                "git_log",
                "checkpoint_list",
                "create_handoff",
            ]
        );
    }

    #[tokio::test]
    async fn disabled_handoff_is_not_advertised_or_callable() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!("req-tools-list-no-handoff")),
            method: "tools/list".into(),
            params: json!({}),
        };
        let response = handle_tools_list_with_show_detail_mode(
            &req,
            Mode::Both,
            ToolMode::ReadOnly,
            false,
            &None,
            ShowDetailMode::Expanded,
        )
        .await;
        let names = response
            .result
            .as_ref()
            .and_then(|result| result.get("tools"))
            .and_then(Value::as_array)
            .expect("missing tools")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "iyunzhi_bi_query",
                "catdesk_instruction",
                "read",
                "search",
                "outline",
                "find_symbol",
                "read_symbol",
                "git_status",
                "git_diff",
                "git_log",
                "checkpoint_list",
            ]
        );

        let workspace_root =
            std::env::temp_dir().join(format!("catdesk-mcp-disabled-handoff-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workspace_root).expect("create workspace");
        let call = tool_call_request("create_handoff", json!({ "goal": "should fail" }));
        let blocked = handle_tools_call_with_show_detail_mode(
            &call,
            &workspace_root.to_string_lossy(),
            1,
            Mode::Both,
            ToolMode::ReadOnly,
            false,
            false,
            false,
            &CommandJobManager::new(),
            &None,
            ShowDetailMode::Expanded,
        )
        .await;
        assert!(result_text(&blocked).contains("Unknown tool: create_handoff"));
        let _ = std::fs::remove_dir_all(workspace_root);
"""
    if idx == 8:
        return ours
    raise RuntimeError(f"unexpected mcp.rs conflict {idx}")

resolve("Cargo.toml", cargo)
resolve("src/process_runner.rs", process_runner)
resolve("src/main.rs", main)
resolve("src/mcp.rs", mcp)

for path in ["Cargo.toml", "src/process_runner.rs", "src/main.rs", "src/mcp.rs"]:
    text = (ROOT / path).read_text()
    if any(marker in text for marker in ("<<<<<<<", "|||||||", ">>>>>>>")):
        raise RuntimeError(f"unresolved conflict marker remains in {path}")
