# CatDesk coding-agent upgrade

This branch turns CatDesk's Computer / multi-tools profile into a workspace an
editing model can actually work in. ChatGPT supplies the model and the
reasoning loop; CatDesk supplies the tools, the limits, and the guarantees.

## Tools

Twenty-two, in five groups.

| Group | Tools |
| --- | --- |
| Read | `read`, `search` |
| Navigate | `outline`, `find_symbol`, `read_symbol` |
| Edit | `write`, `edit`, `delete`, `apply_patch` |
| Undo | `checkpoint_list`, `checkpoint_restore` |
| Run | `run_command`, `start_command`, `poll_command`, `cancel_command`, `run_checks` |
| Git | `git_status`, `git_diff`, `git_log`, `git_add`, `git_commit` |

Plus `catdesk_instruction`. In ReadOnly mode only the read, navigate,
`checkpoint_list` and read-only Git tools are advertised.

### Navigate

`read` is capped at 32 KiB per file. `src/mcp.rs` in this repository is
318 KiB, so the tool that edits it could see 10% of it. The navigation tools
close that gap:

- `outline` lists a file's definitions with line numbers and nesting.
  On `mcp.rs`: 264 symbols in 85 ms, a 14.9 KiB answer — 4.7% of the file.
- `find_symbol` locates a definition anywhere under a workspace directory.
  Candidate files are filtered on a plain substring before any of them is
  parsed, so a workspace-wide search stays cheap.
- `read_symbol` returns one definition's source. `handle_run_checks` comes
  back as its own 5 KiB rather than a 32 KiB window that may not contain it.

Rust, Python, JavaScript, TypeScript and Go, parsed with tree-sitter. A
brace-matching heuristic is defeated by braces inside string literals — which
is most of what a file full of `json!` macros contains — and a wrong symbol
range would send an edit to the wrong lines.

`outline` answers with a rendered outline, one symbol per line. The per-symbol
array with byte and line ranges is available with `include_symbols: true`; it
is off by default because as JSON the same 264 symbols cost 50.6 KiB, which is
more than reading the truncated file it replaces.

### Run

`run_checks` runs the project's tests, build or type check and returns the
verdict: pass/fail counts, the failing test names, and file/line diagnostics.

Routing a test run through `run_command` does not work. That buffer keeps the
first 32 KiB and a runner prints its summary last, so the answer is exactly the
part that gets dropped. `run_checks` captures up to 4 MiB, parses it here, and
returns the result plus a bounded tail.

Parsers for cargo, pytest, go and node/tsc, detected from the project's marker
files, with a generic fallback and an explicit `command` override. Timeout
defaults to 5 minutes rather than borrowing `run_command`'s 120-second ceiling.

A red test run is a **successful call with a red verdict**, not a tool error.
Only a run that produced no verdict at all — exit 127, or a timeout — is
reported as an error, so a failing suite cannot look like a transport problem
worth retrying.

### Undo

`write`, `edit`, `delete` and `apply_patch` record the files they are about to
touch before touching them. `checkpoint_list` shows the recordings and
`checkpoint_restore` puts the files back. The workspace is often not a clean
Git tree, or not a repository at all, so "just use git" is not an answer.

Bounded by file size (4 MiB), entry count (500), per-checkpoint size (16 MiB),
history depth (12) and total history (128 MiB).

`run_command` is **not** checkpointed, and the tool description says so. What a
shell command will touch is not knowable before it runs, and snapshotting the
whole workspace on every call would cost far more than it saves.

Checkpoint manifests live in the workspace and are therefore reachable by the
very tools this protects against, so paths read back out of them are re-checked
before anything is written or removed.

### Edit and Git

`apply_patch` takes a workspace-relative `cwd`, checks the paths in the
`diff --git`, `---`/`+++`, rename and copy headers, dry-runs `git apply
--check`, and only then applies. Git's own unsafe-path refusal remains the
backstop.

Git paths are confined to the workspace. `git_commit` uses the repository or
user identity when one is configured and supplies a CatDesk-local fallback
otherwise, which is the normal state for a service account.

## Safety

Enforced in code, not by instruction:

- Destructive shell forms are refused: `git reset --hard/--merge/--keep`,
  `checkout` with a pathspec or `--force`, `restore` without `--staged`,
  `clean` without `--dry-run`, mutating `stash`, `branch -D`, force-push, and
  recursive `rm`. Branch switching, unstaging, `git stash list/show/apply/pop`
  and `git clean --dry-run` all still work: refusing a whole subcommand would
  make ordinary development impossible through the tool.
- Command output is capped at 32 KiB, poll pages at 32 KiB, reads at 32 KiB per
  file and 64 KiB per batch, list/search defaults at 100/50.
- Linux commands may read the standard runtime paths and `/proc`. The
  filesystem root is not granted.
- Workspace writes stay sandboxed; the systemd unit confines writes to
  `/srv/catdesk` independently of anything the tools do.

The operating guidance still asks for an inspect-plan-act-verify loop, bounded
retries and checkpoint notes. That part is advice. The list above is not.

## Runtime

Reqwest uses its provider-neutral Rustls feature and CatDesk installs ngrok's
AWS-LC provider before building the router. Enabling both Ring and AWS-LC
panicked during TLS initialization. The binary has no dynamic `libssl` or
`libcrypto` dependency.

## Build

The toolchain lives in the workspace, not on the default PATH:

```bash
export CARGO_HOME=/srv/catdesk/workspace/.rustcargo
export RUSTUP_HOME=/srv/catdesk/workspace/.rustup
export PATH="$CARGO_HOME/bin:$PATH"
cd /srv/catdesk/workspace/catdesk-src
cargo test --release --offline
cargo build --release --offline
```

`rustfmt` is installed; `clippy` is not. `src/browser.rs` carries one
pre-existing formatting difference from upstream that this branch leaves alone.

## Deploy

```bash
sha256sum target/release/catdesk
sudo /srv/catdesk/workspace/deploy-catdesk-custom.sh <expected-sha256>
```

The script takes the expected SHA as an argument rather than pinning one build.
It verifies the SHA and the ARM64 architecture, backs up the live binary,
replaces it atomically, requires twelve consecutive healthy seconds, runs the
live MCP probe, and rolls back automatically on any failure.

## Verification

`verify-agent-live.py` exercises the running service, not a unit-test double:

- every tool is advertised, and the operating guidance mentions the new ones
- the 32 KiB command output guard holds
- destructive commands are refused and `git checkout -b` still works
- a real repository round trip: write, stage, commit, patch, diff, log
- a checkpoint round trip: overwrite a file and get the original back
- a check verdict: counts, failing test name and `file:line` out of a
  transcript
- a code navigation round trip: outline a file, find a definition, read it
- a background command start/poll/drain

It runs against a repository it creates, so a service account's missing Git
identity cannot pass unnoticed the way it did when the probes used a
pre-configured temporary repo.

## What this is not

CatDesk supplies the tool runtime. ChatGPT Web supplies the model and the agent
loop, which means the context window, compaction, retry policy and connector
tool-list caching are all outside CatDesk's control. This approximates a
coding agent's workflow; it does not embed one.
