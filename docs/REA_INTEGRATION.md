# Optional REA skill for CatDesk

This integration adds one small read-only CatDesk tool, `load_skill`, that lazily returns the embedded `rea` workflow instructions **only when requested**. The skill is sourced from the user's fork at [ericpq/rea](https://github.com/ericpq/rea). It does not add REA's large MCP tool catalog to ChatGPT's always-on context.

## Architecture

```text
ChatGPT → CatDesk MCP (unchanged Agent / tool permissions)
             ├── load_skill(name="rea") → embedded workflow text
             └── existing run_command/start_command (on demand)
                       └── separately installed REA CLI
                                 └── optional Ghidra / JADX / ADB / etc.
```

The binary embeds `skills/rea/SKILL.md` at **build time**. No extra skill file is required at runtime. The entry point is read-only and does not install packages, spawn REA or mutate the user's environment. CatDesk remains the command-execution and workspace security boundary. The user's REA fork is the reference for the full skill and its individual guides; REA source is not vendored into CatDesk.

## Usage

When an authorized task requires reverse engineering, call `load_skill` with `{"name":"rea"}`. Follow `skillText` returned by the tool and check the actual REA installation on the execution host. Existing CatDesk read/search/Git tools remain the preferred choice for source-only projects. If no `rea` CLI is available, report it as a prerequisite; do not install anything on the production KR host without a separately reviewed change.

## Runtime prerequisites

- **Static JavaScript/Electron**: compatible Node.js and REA CLI. REA's Node.js support is 22.19+ in 22.x, 24.11+ in 24.x, and 26+.
- **Android APK classes/methods**: compatible full JDK plus headless JADX bridge. For device access supply an existing ADB executable and an authorized device.
- **Native binaries**: a supported Ghidra, Hopper or IDA installation, configured separately. OS/CPU architecture availability depends on the analysis provider.
- Install and pin REA from an **explicitly reviewed** source/revision; the `ericpq/rea` fork and registry package can differ. Inspect `rea --version` and `rea --help` before using version-specific commands.

## Security and operational boundaries

1. No automatic dependencies, no auto-cloning, no globally registered REA MCP process, no ongoing polling jobs.
2. Analyze only authorized artifacts placed within CatDesk's permitted workspace. The skill does not elevate filesystem or process privileges.
3. Prefer offline/static modes before runtime observation; runtime capture and on-device operations have additional effects and require the corresponding user authorization.
4. Apply the existing CatDesk Agent verification/checkpoint requirements to any resulting code change. Preserve REA Evidence IDs, uncertainty and resource limits.
5. Do **not** copy private cinema BI integrations, credentials or server-specific configuration into either public repo.

## Validation and rollout

- Build-time embedded skill must exist and contain REA's expected routes.
- `load_skill(name="rea")` should return `skillText` without starting REA.
- Unknown skill names should fail clearly.
- Read-only tool mode should still allow the skill loader.
- Existing tools-list, output-schema, Agent and MCP tests must remain green.

Merge the GitHub feature branch only after a Rust test/build review. Deploy through the existing verified CatDesk release/rollback procedure; **do not swap a running KR binary just to add an untested skill**. Loading the skill is not evidence that REA is installed or operational.
