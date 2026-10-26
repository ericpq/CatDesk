---
name: rea
description: On-demand reverse engineering of authorized binaries, APKs, Electron applications and web artifacts through the separately installed REA CLI.
source: https://github.com/ericpq/rea
---

# REA / Reverse Engineer Anything — CatDesk skill

This is the CatDesk **on-demand** entry point for the user's fork of REA:
https://github.com/ericpq/rea

REA is a separate project and process. Loading this skill does **not** install REA, start its MCP server, expose its entire tool catalog, install Ghidra/JADX, or grant access outside the CatDesk workspace. For detailed procedures use the user's fork's original [REA skill](https://github.com/ericpq/rea/blob/main/.agents/skills/reverse-engineer-anything/SKILL.md) and its references. Check the actual installed REA version and available CLI commands: fork main, published npm releases and the installed binary may differ.

## When to use

Load this skill only when the user wants to understand or compare behavior unavailable in source: native binaries, Android APKs, packaged Electron/JavaScript, managed assemblies, firmware, WASM, or browser/network capture artifacts. For ordinary source-repository review, use CatDesk's native read/search/outline/Git tools instead; do not invoke REA.

## Scope and prerequisites

1. Identify the user-supplied target, question, authorized analysis scope and actual accessible file path. Do not guess an app or examine unrelated files. Stage local artifacts **inside the configured CatDesk workspace**; do not bypass CatDesk's existing workspace/sandbox/access limits.
2. Before attempting an operation, check `command -v rea` and `rea --version` using the existing CatDesk command tools. If unavailable, report the missing runtime; do not silently install `rea-agents`, Node, Java, Ghidra, JADX, Hopper, IDA, Android SDK or other dependencies. Any installation or change to persistent MCP registration needs separately authorized, reviewed steps.
3. REA supports Node.js >=22.19 on the 22 line, >=24.11 on the 24 line, or 26+; other tools vary. REA's static JavaScript analysis needs only Node/npm. Android class/method analysis requires a compatible full JDK plus a caller-provided headless JADX bridge; deep native analysis needs a supported, separately provisioned Hopper/Ghidra/IDA engine. ADB needs a caller-provided `adb` and an authorized connected device. Confirm provider/host support first, especially on Linux ARM64.
4. Keep executable and native-runtime analysis separate from static inspection. Do not execute an untrusted target, attach a browser/device, install apps, trigger interactive lifecycle actions, or capture traffic without the request's actual authorization and user awareness. No credential extraction, paywall/license bypass, or unauthorized access.

## Using REA from CatDesk

Use CatDesk's existing `run_command` for short, targeted CLI operations. For long jobs use `start_command`, `poll_command`, and `parse_checks` where applicable. Commands run under CatDesk's existing limits, not with new privileges.

Start with the smallest appropriate command; examples (replace placeholders only with the real, authorized path):

```sh
rea --version
rea --help
rea analyze-javascript-application ./target/extracted-app --json
rea inspect-android-package ./target/example.apk
rea inspect-adb-client
rea list-adb-devices
```

Use `rea doctor` or `rea --help` only when troubleshooting a concrete blocker, not before every task. If a CLI subcommand differs in the installed version, use that version's built-in help instead of assuming fork/main documentation is available. For non-interactive Ghidra or native analysis, verify the selected engine is installed and compatible before using `rea open-binary` or other provider-specific commands. Do not start a persistent REA MCP server just to answer a single question.

REA commands may emit large JSON. Use summary views / target-specific filters, or save output in an approved workspace file and read focused ranges with CatDesk `read`, `search` or `read_symbol`. Never flood the ChatGPT context with a full archive or Evidence bundle. Do not retain sensitive captures in public repositories. Reuse saved evidence rather than rerunning the same heavy analysis.

## Reporting and completion

Identify the exact artifact/version, its SHA-256 if available, and the operation/provider actually used. Separate observed evidence from inferred behavior and unknowns. Cite REA Evidence identifiers or paths when returned. Static decompilation is not proof of runtime behavior. Verify reconstructed code independently if asked to implement a feature. Close sessions and clean up temporary files belonging to the task without deleting originals.

**Critical:** REA and CatDesk remain separately upgradeable. This skill must not change the active KR CatDesk service, model/bridge hooks, existing agent tool policy or private BI sidecars.
