from pathlib import Path

path = Path("src/mcp.rs")
text = path.read_text()

replacements = [
    (
        """        &command_text,
        Path::new(workspace_root),
        cwd,
        command::MAX_TIMEOUT_MS,
""",
        """        &command_text,
        Path::new(workspace_root),
        cwd,
        true,
        command::MAX_TIMEOUT_MS,
""",
    ),
    (
        """        &format!("git config --get {}", shell_quote(key)),
        Path::new(workspace_root),
        cwd,
        command::MAX_TIMEOUT_MS,
""",
        """        &format!("git config --get {}", shell_quote(key)),
        Path::new(workspace_root),
        cwd,
        true,
        command::MAX_TIMEOUT_MS,
""",
    ),
    (
        """        &check_cmd,
        Path::new(workspace_root),
        &cwd,
        command::MAX_TIMEOUT_MS,
""",
        """        &check_cmd,
        Path::new(workspace_root),
        &cwd,
        true,
        command::MAX_TIMEOUT_MS,
""",
    ),
    (
        """        &apply_cmd,
        Path::new(workspace_root),
        &cwd,
        command::MAX_TIMEOUT_MS,
""",
        """        &apply_cmd,
        Path::new(workspace_root),
        &cwd,
        true,
        command::MAX_TIMEOUT_MS,
""",
    ),
    (
        """        &command_text,
        Path::new(workspace_root),
        &cwd,
        timeout_ms,
        checks::MAX_CAPTURE_BYTES,
""",
        """        &command_text,
        Path::new(workspace_root),
        &cwd,
        true,
        timeout_ms,
        checks::MAX_CAPTURE_BYTES,
""",
    ),
    (
        """        cmd,
        Path::new("/"),
        &cwd,
        command::clamp_timeout(timeout_ms),
""",
        """        cmd,
        Path::new("/"),
        &cwd,
        false,
        command::clamp_timeout(timeout_ms),
""",
    ),
]

for old, new in replacements:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"expected one command-signature compatibility site, found {count}: {old!r}")
    text = text.replace(old, new, 1)

path.write_text(text)
print(f"patched {len(replacements)} CatDesk 0.9.3 command signatures")
