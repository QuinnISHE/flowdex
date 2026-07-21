# Flowdex Batch 003: Silent verification

The frozen `flowdex` bootstrap exposes `verify` to saved JavaScript workflows when the normal command-execution capability is available. It is a hidden workflow operation: available inside a running workflow, not a direct model tool, and unavailable when command execution is unavailable.

## API

```js
const result = await flowdex.verify(
  ["git diff --check", "just test -p codex-flowdex"],
  { workdir: "codex-rs", timeoutMs: 120_000 },
);
```

`commands` must be a non-empty array of non-empty strings. The only options are `workdir` (an optional repository-relative working directory; by default the workflow turn's current environment directory) and `timeoutMs` (an optional timeout applied independently to each command; omission uses the normal shell-command default). Verification inherits the ordinary shell, environment, permissions, sandbox, approval, hooks, cancellation, and output limits.

Commands run sequentially in declaration order and stop after the first non-zero exit or timeout. Each result is either:

```js
{ passed: true, commands: [
  { command: "git diff --check", exitCode: 0, durationMs: 21 },
] }
```

or:

```js
{ passed: false, commands: [
  { command: "git diff --check", exitCode: 1, durationMs: 18, output: "..." },
] }
```

Each executed entry has `command`, `exitCode`, and integer `durationMs`. A timed-out entry additionally has `timedOut: true`. Failed or timed-out entries may include bounded aggregated stdout/stderr as `output`; passing output is omitted, as is empty output. Non-zero exits and timeouts are returned as results, not JavaScript exceptions. Invalid arguments, an invalid working directory, denied execution, cancellation, or an internal execution error are tool errors.

## Silent behavior and repair composition

Silent verification suppresses an intermediate model turn and passing verification output in the parent model context. It does not suppress normal user-visible command lifecycle events. On failure, bounded output is returned to JavaScript; Rust does not choose an agent or repair automatically. A workflow can route the result explicitly:

Scheduled task workers receive the exact task verification command list with an explicit note that Flowdex runs it after their turn. Workers should not rerun those commands merely to complete the workflow verification step. When automatic repair is enabled, Flowdex sends the bounded failed result to the same task agent and reruns verification after the repair.

```js
const result = await flowdex.verify(commands);
if (!result.passed) {
  await flowdex.sendMessage(agentId, JSON.stringify(result), { delivery: "turn" });
}
```

## Current limits

This batch provides only the verification primitive. It adds no tasks, phases, automatic repair loop or budgets, reviews, worktrees, commits, attribution, SQLite state, context chunks or compaction, suspension or steering, configuration, or scheduler.
