use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccessLevel {
    Read,
    Write,
    Admin,
}

impl AccessLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ToolPolicy {
    pub name: &'static str,
    pub category: &'static str,
    pub access: AccessLevel,
    pub retry: &'static str,
    pub verify: &'static str,
}

const LOCAL_TOOLS: &[ToolPolicy] = &[
    ToolPolicy {
        name: "catdesk_instruction",
        category: "agent",
        access: AccessLevel::Read,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "agent_status",
        category: "agent",
        access: AccessLevel::Read,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "agent_checkpoint",
        category: "agent",
        access: AccessLevel::Write,
        retry: "never",
        verify: "result",
    },
    ToolPolicy {
        name: "agent_plan",
        category: "agent",
        access: AccessLevel::Write,
        retry: "never",
        verify: "result",
    },
    ToolPolicy {
        name: "agent_recover",
        category: "safety",
        access: AccessLevel::Admin,
        retry: "never",
        verify: "result",
    },
    ToolPolicy {
        name: "read",
        category: "workspace",
        access: AccessLevel::Read,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "search",
        category: "workspace",
        access: AccessLevel::Read,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "outline",
        category: "workspace",
        access: AccessLevel::Read,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "find_symbol",
        category: "workspace",
        access: AccessLevel::Read,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "read_symbol",
        category: "workspace",
        access: AccessLevel::Read,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "write",
        category: "workspace",
        access: AccessLevel::Write,
        retry: "never",
        verify: "observable_state",
    },
    ToolPolicy {
        name: "edit",
        category: "workspace",
        access: AccessLevel::Write,
        retry: "never",
        verify: "observable_state",
    },
    ToolPolicy {
        name: "apply_patch",
        category: "workspace",
        access: AccessLevel::Write,
        retry: "never",
        verify: "observable_state",
    },
    ToolPolicy {
        name: "delete",
        category: "workspace",
        access: AccessLevel::Admin,
        retry: "never",
        verify: "observable_state",
    },
    ToolPolicy {
        name: "checkpoint_list",
        category: "safety",
        access: AccessLevel::Read,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "checkpoint_restore",
        category: "safety",
        access: AccessLevel::Admin,
        retry: "never",
        verify: "observable_state",
    },
    ToolPolicy {
        name: "run_command",
        category: "shell",
        access: AccessLevel::Write,
        retry: "never",
        verify: "result",
    },
    ToolPolicy {
        name: "root_command",
        category: "shell",
        access: AccessLevel::Admin,
        retry: "never",
        verify: "observable_state",
    },
    ToolPolicy {
        name: "start_command",
        category: "shell",
        access: AccessLevel::Write,
        retry: "never",
        verify: "result",
    },
    ToolPolicy {
        name: "poll_command",
        category: "shell",
        access: AccessLevel::Write,
        retry: "never",
        verify: "result",
    },
    ToolPolicy {
        name: "cancel_command",
        category: "shell",
        access: AccessLevel::Write,
        retry: "never",
        verify: "result",
    },
    ToolPolicy {
        name: "run_checks",
        category: "verification",
        access: AccessLevel::Write,
        retry: "never",
        verify: "result",
    },
    ToolPolicy {
        name: "parse_checks",
        category: "verification",
        access: AccessLevel::Write,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "git_status",
        category: "git",
        access: AccessLevel::Read,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "git_diff",
        category: "git",
        access: AccessLevel::Read,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "git_log",
        category: "git",
        access: AccessLevel::Read,
        retry: "never",
        verify: "none",
    },
    ToolPolicy {
        name: "git_add",
        category: "git",
        access: AccessLevel::Write,
        retry: "never",
        verify: "result",
    },
    ToolPolicy {
        name: "git_commit",
        category: "git",
        access: AccessLevel::Admin,
        retry: "never",
        verify: "observable_state",
    },
];

pub fn local_policy(name: &str) -> Option<&'static ToolPolicy> {
    LOCAL_TOOLS.iter().find(|policy| policy.name == name)
}

fn access_from_annotations(tool: &Value) -> AccessLevel {
    let Some(annotations) = tool.get("annotations").and_then(Value::as_object) else {
        return AccessLevel::Admin;
    };
    if annotations
        .get("destructiveHint")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return AccessLevel::Admin;
    }
    if annotations
        .get("readOnlyHint")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return AccessLevel::Read;
    }
    AccessLevel::Write
}

pub fn descriptor_access(tool: &Value) -> AccessLevel {
    tool.get("name")
        .and_then(Value::as_str)
        .and_then(local_policy)
        .map(|policy| policy.access)
        .unwrap_or_else(|| access_from_annotations(tool))
}

pub fn descriptor_retry(tool: &Value) -> &'static str {
    if let Some(policy) = tool
        .get("name")
        .and_then(Value::as_str)
        .and_then(local_policy)
    {
        return policy.retry;
    }
    let open_world = tool
        .get("annotations")
        .and_then(|value| value.get("openWorldHint"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if descriptor_access(tool) == AccessLevel::Read && open_world {
        "transient_once"
    } else {
        "never"
    }
}

pub fn descriptor_verify(tool: &Value) -> &'static str {
    if let Some(policy) = tool
        .get("name")
        .and_then(Value::as_str)
        .and_then(local_policy)
    {
        return policy.verify;
    }
    if descriptor_access(tool) == AccessLevel::Read {
        "result"
    } else {
        "observable_state"
    }
}

fn parse_access(value: Option<&str>) -> AccessLevel {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None => AccessLevel::Admin,
        Some("read") | Some("reader") | Some("readonly") | Some("read-only") => AccessLevel::Read,
        Some("write") | Some("operator") => AccessLevel::Write,
        Some("admin") => AccessLevel::Admin,
        Some(_) => AccessLevel::Read,
    }
}

pub fn configured_max_access() -> AccessLevel {
    parse_access(std::env::var("CATDESK_MAX_TOOL_ACCESS").ok().as_deref())
}

pub fn effective_max_access(read_only_mode: bool) -> AccessLevel {
    if read_only_mode {
        AccessLevel::Read
    } else {
        configured_max_access()
    }
}

fn allows_with_max(access: AccessLevel, read_only_mode: bool, configured: AccessLevel) -> bool {
    let max = if read_only_mode {
        AccessLevel::Read
    } else {
        configured
    };
    access <= max
}

pub fn allows(access: AccessLevel, read_only_mode: bool) -> bool {
    allows_with_max(access, read_only_mode, configured_max_access())
}

pub fn descriptor_allowed(tool: &Value, read_only_mode: bool) -> bool {
    allows(descriptor_access(tool), read_only_mode)
}

pub fn attach_policy_meta(tool: &mut Value) {
    let Some(name) = tool.get("name").and_then(Value::as_str).map(str::to_string) else {
        return;
    };

    let (category, access, retry, verify) = if let Some(policy) = local_policy(&name) {
        (policy.category, policy.access, policy.retry, policy.verify)
    } else {
        (
            "browser",
            descriptor_access(tool),
            descriptor_retry(tool),
            descriptor_verify(tool),
        )
    };

    let Some(object) = tool.as_object_mut() else {
        return;
    };
    let meta = object
        .entry("_meta".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(meta) = meta.as_object_mut() else {
        return;
    };
    meta.insert(
        "catdesk/toolPolicy".to_string(),
        json!({
            "category": category,
            "access": access.as_str(),
            "retry": retry,
            "verify": verify,
        }),
    );
}

pub fn summary(read_only_mode: bool) -> Value {
    let mut read = 0usize;
    let mut write = 0usize;
    let mut admin = 0usize;
    for policy in LOCAL_TOOLS {
        match policy.access {
            AccessLevel::Read => read += 1,
            AccessLevel::Write => write += 1,
            AccessLevel::Admin => admin += 1,
        }
    }
    json!({
        "maxAccess": effective_max_access(read_only_mode).as_str(),
        "configuredMaxAccess": configured_max_access().as_str(),
        "localRegistry": {
            "total": LOCAL_TOOLS.len(),
            "read": read,
            "write": write,
            "admin": admin,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_classifies_risky_tools_without_guessing() {
        assert_eq!(local_policy("read").unwrap().access, AccessLevel::Read);
        assert_eq!(local_policy("write").unwrap().access, AccessLevel::Write);
        assert_eq!(
            local_policy("root_command").unwrap().access,
            AccessLevel::Admin
        );
        assert_eq!(
            local_policy("agent_plan").unwrap().access,
            AccessLevel::Write
        );
        assert_eq!(
            local_policy("agent_recover").unwrap().access,
            AccessLevel::Admin
        );
    }

    #[test]
    fn dynamic_descriptor_uses_mcp_annotations_fail_closed() {
        let read = json!({
            "name": "browser_read",
            "annotations": { "readOnlyHint": true, "openWorldHint": true, "destructiveHint": false }
        });
        let destructive = json!({
            "name": "browser_write",
            "annotations": { "readOnlyHint": false, "openWorldHint": true, "destructiveHint": true }
        });
        let contradictory = json!({
            "name": "browser_trick",
            "annotations": { "readOnlyHint": true, "destructiveHint": true }
        });
        let missing = json!({ "name": "browser_unknown" });
        assert_eq!(descriptor_access(&read), AccessLevel::Read);
        assert_eq!(descriptor_retry(&read), "transient_once");
        assert_eq!(descriptor_verify(&read), "result");
        assert_eq!(descriptor_access(&destructive), AccessLevel::Admin);
        assert_eq!(descriptor_retry(&destructive), "never");
        assert_eq!(descriptor_verify(&destructive), "observable_state");
        assert_eq!(descriptor_access(&contradictory), AccessLevel::Admin);
        assert_eq!(descriptor_access(&missing), AccessLevel::Admin);
    }

    #[test]
    fn access_parser_is_backward_compatible_but_typos_fail_closed() {
        assert_eq!(parse_access(None), AccessLevel::Admin);
        assert_eq!(parse_access(Some("reader")), AccessLevel::Read);
        assert_eq!(parse_access(Some("operator")), AccessLevel::Write);
        assert_eq!(parse_access(Some("admin")), AccessLevel::Admin);
        assert_eq!(parse_access(Some("admn")), AccessLevel::Read);
        assert_eq!(parse_access(Some("")), AccessLevel::Read);
    }

    #[test]
    fn permission_ladder_cannot_be_bypassed_by_direct_calls() {
        assert!(allows_with_max(AccessLevel::Read, false, AccessLevel::Read));
        assert!(!allows_with_max(
            AccessLevel::Write,
            false,
            AccessLevel::Read
        ));
        assert!(!allows_with_max(
            AccessLevel::Admin,
            false,
            AccessLevel::Write
        ));
        assert!(allows_with_max(
            AccessLevel::Write,
            false,
            AccessLevel::Write
        ));
        assert!(!allows_with_max(
            AccessLevel::Write,
            true,
            AccessLevel::Admin
        ));
        assert!(!allows_with_max(
            AccessLevel::Admin,
            true,
            AccessLevel::Admin
        ));
    }

    #[test]
    fn registry_overwrites_spoofed_policy_metadata() {
        let mut tool = json!({
            "name": "delete",
            "annotations": { "readOnlyHint": false, "destructiveHint": true },
            "_meta": {
                "catdesk/toolPolicy": {
                    "category": "fake",
                    "access": "read",
                    "retry": "transient_once",
                    "verify": "none"
                }
            }
        });
        attach_policy_meta(&mut tool);
        let policy = &tool["_meta"]["catdesk/toolPolicy"];
        assert_eq!(policy["category"], "workspace");
        assert_eq!(policy["access"], "admin");
        assert_eq!(policy["retry"], "never");
        assert_eq!(policy["verify"], "observable_state");
    }
}
