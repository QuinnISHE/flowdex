# Flowdex

Flowdex is a modified Codex CLI for running model-authored coding workflows. It moves routine orchestration out of the main model loop: a saved JavaScript workflow can dispatch agents, wait on events, verify changes, route review findings, and continue through dependencies without repeatedly waking the orchestrator.

Workflows run in Codex's native V8 runtime and require no Node.js installation.

## What it adds

- Durable runs composed of phases and dependency-aware tasks.
- Parallel agents working in isolated Git worktrees with commit attribution.
- Silent command verification and line-attributed review/repair rounds.
- Event-driven waiting that still wakes for user steering, boundaries, and named signals.
- Reusable repository or global workflows with strict JSON inputs and outputs.
- Context packs that deliver refreshable source fragments directly to dependent agents.
- Automatic, nonpersistent progress summaries and normal app-visible subagent events.
- Native context compaction, per-agent tool profiles, and repository AST-grep rules.

See the [getting-started guide](flowdex-plan/flowdex-documentation/getting-started.md), [workflow documentation](flowdex-plan/flowdex-documentation/workflows.md), and [video demo](flowdex-demo/VIDEO_DEMO.md).

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
```
