# CatDesk coding-agent upgrade

This branch adds a bounded coding-agent workflow to CatDesk's Computer / multi-tools
profile. ChatGPT supplies the model and reasoning loop; CatDesk supplies the local
workspace, command, patch, and Git tools.

## Tooling and safety

- Workspace tools: `read`, `search`, `write`, `edit`, `delete`, `apply_patch`
- Command tools: `run_command`, `start_command`, `poll_command`, `cancel_command`
- Git tools: `git_status`, `git_diff`, `git_log`, `git_add`, `git_commit`
- `apply_patch` accepts a workspace-relative `cwd`, validates normal and rename/copy
  headers, runs `git apply --check`, and then applies the patch.
- Git paths are confined to the workspace. `git_commit` uses the repository or user
  identity when configured and a CatDesk-local fallback identity otherwise.
- Shell `git reset`, `checkout`, `clean`, `stash`, force-push, and recursive `rm` are
  rejected. Use the dedicated scoped tools for intentional changes.
- Linux commands can read standard runtime paths and `/proc`, but do not receive a
  blanket read grant for `/`.
- Synchronous command output is capped at 32 KiB. File reads are capped at 32 KiB per
  file and 64 KiB per call.

## Korea build environment

The host does not expose Cargo on the default service shell path. Use the workspace
toolchain explicitly:

```bash
export CARGO_HOME=/srv/catdesk/workspace/.rustcargo
export RUSTUP_HOME=/srv/catdesk/workspace/.rustup
export PATH="$CARGO_HOME/bin:$PATH"
cd /srv/catdesk/workspace/catdesk-src
cargo test --release --locked
cargo build --release --locked
```

The release binary is `target/release/catdesk`. Deploy with the reusable fail-closed
script and the SHA256 of that exact build:

```bash
sha256sum target/release/catdesk
sudo /srv/catdesk/workspace/deploy-catdesk-custom.sh <expected-sha256>
```

The deployment script verifies SHA and ARM64 architecture, takes a backup, performs an
atomic replacement, requires sustained service health, runs the live MCP workflow
probe, and rolls back automatically on failure.
