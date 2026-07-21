# Flowdex Batch 021: Release readiness

## Goal

Make a packaged Flowdex install usable on a clean Windows or macOS machine without relying on Quinn's local profiles or shell session.

## Changes

### Release package

- Keep the optimized release build.
- On Windows, build and package `flowdex.exe`, `codex-windows-sandbox-setup.exe`, and `codex-command-runner.exe`; the installer already requires all three beside the main binary.
- On macOS, package the Flowdex binary for the existing supported architectures.
- Make release notes and package documentation describe the actual artifact contents.

### Installed defaults

- Add `multi_agent_version = "v1"` to the bundled global `flowdex.toml` so the bundled Luna reviewer works across models on a fresh install. User or trusted-repository config can still select `v2`.
- Make bundled workflows self-contained. Do not require a custom `.codex/agents` profile that the package does not install.
- Keep the reviewer on `gpt-5.6-luna`; use an explicit available model/reasoning selector for the worker.
- Correct skill/docs examples to use `global:defaults/...` for installed defaults, while noting `repo:defaults/...` only when the repository contains its own copy.

### Exact messaging rounds

- Preserve general agent-to-agent messaging in the research-rounds example.
- Do not use `sendMessage(..., { delivery: "turn" })` followed by `waitAgent()`, because that can observe the previous terminal operation.
- Queue the peer message and use `resumeAgent(..., { contextMode: "keep" })` as the exact submitted operation that advances and awaits the recipient.
- Keep the numeric round budget and role-neutral agents.

### macOS Codex app integration

- Replace shell-profile-only persistence with a per-user LaunchAgent that sets `CODEX_CLI_PATH` for the GUI login session.
- Install/update the LaunchAgent safely, apply the value to the current GUI session with `launchctl`, and tell the user to fully restart Codex.
- Uninstall must unload/remove the LaunchAgent and unset the GUI-session variable. Do not modify PATH or copy over Codex's original backend.
- Keep file generation and command execution injectable/testable without a live macOS host. Do not add a second installer framework.

## Verification

- Focused CLI installer tests for package validation, installed default content, macOS LaunchAgent install/uninstall behavior, and idempotence.
- Validate the default workflow JavaScript contract with existing focused Flowdex tests or the narrowest available loader/runtime test.
- Inspect the release workflow's Windows artifact list.
- Run formatting and `git diff --check`.
- Do not run a full release build or full workspace suite.

## Non-goals

- No new workflow runtime APIs, compatibility shims, feature flags, alternate agent lifecycle, release auto-update service, or broad installer rewrite.
- Do not change the accepted Batch 020 boundary/context behavior.
