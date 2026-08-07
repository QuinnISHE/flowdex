# Flowdex

Flowdex is a modified Codex CLI for running model-authored coding workflows. It takes the model out of the loop for trivial orchestration decisions, and instead only uses Codex models when their input is genuinely needed.

Workflows run in Codex's native V8 runtime and require no Node.js installation.

## Features

- Durable runs composed of phases and dependency-aware tasks.
- Parallel agents working in isolated Git worktrees with commit attribution.
- Silent command verification and line-attributed review/repair rounds.
- Event-driven waiting that still wakes for user steering, boundaries, and named signals.
- Reusable repository or global workflows with strict JSON inputs and outputs.
- Context packs that deliver refreshable source fragments directly to dependent agents.
- Automatic, nonpersistent progress summaries and normal app-visible subagent events.
- Native context compaction, per-agent tool profiles, and repository AST-grep rules.

See the release to download the binary, along with a demo that will create an HTML file displaying an animated Flowdex logo.

## Build

Install the normal Codex build prerequisites, then build the modified CLI from the Rust workspace:

```shell
cd codex-rs
cargo build --release -p codex-cli --bin codex
```

The executable is written to `codex-rs/target/release/codex` on macOS or `codex-rs/target/release/codex.exe` on Windows. Copy it into [`flowdex-package`](flowdex-package/README.md) as `flowdex` or `flowdex.exe`.

Run the packaged binary and restart the Codex app:

```shell
flowdex install
flowdex uninstall
flowdex uninstall --purge
```

Normal uninstall preserves Flowdex workflows and configuration. `--purge` also removes global Flowdex data from `CODEX_HOME`; repository `.flowdex/` directories remain project-owned.

## Automated upstream releases

The `Flowdex upstream release` workflow checks the latest stable official Codex release every six hours. Git performs a three-way merge, then Windows and macOS release builds run in parallel. Only a fully passing integration fast-forwards `main` and publishes checksummed archives.

No OpenAI API key is required. If a new upstream conflict cannot be merged mechanically, or a platform build fails, the workflow preserves the useful state and opens a deduplicated GitHub issue with the exact tag, branch, run, and diagnostics. After the issue is resolved on `main`, the next scheduled run completes the release automatically.
