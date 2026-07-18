# Flowdex Implementation Plan 003: Silent Verification

## Outcome

A running Flowdex workflow can execute an ordered set of repository verification commands and receive one structured pass/fail result without waking the parent model between commands.

This is a primitive, not an automatic worker/reviewer loop. JavaScript remains responsible for deciding whether a failure should be sent to an agent, retried, escalated, or returned. That keeps verification separate from review and avoids introducing the task/phase scheduler before it is needed.

## Starting Point

Batch 002 established:

- `flowdex.spawnAgent(...)`, `flowdex.sendMessage(...)`, and `flowdex.waitAgent(...)` as private workflow-only operations.
- Flowdex nested tools use `ToolExposure::Hidden` and are explicitly added only to the saved-workflow tool set.
- Agent completion can return through the workflow without injecting a completion message into the parent model context.
- Nested tool calls inherit the current session, turn, environment, cancellation, sandbox, and approval context.

Codex already has a shared shell execution path through `ShellRuntime` and `ToolOrchestrator`. It resolves the current environment and working directory, applies sandbox and approval policy, runs hooks, emits command lifecycle events, bounds captured output, and supports cancellation. Flowdex must reuse that path rather than starting processes directly.

## Data Transformation

The complete batch is one transformation:

```text
JavaScript command strings + shared options
    -> existing Codex shell execution request for each command
    -> bounded structured command outcomes
    -> one verification result returned to JavaScript
```

Commands run sequentially in declaration order. Stop after the first failed or timed-out command. This preserves the common build-then-test dependency, avoids spending time after a prerequisite fails, and keeps the first version deterministic. A workflow can call `verify` more than once when it has independent command groups.

## JavaScript Contract Introduced

Add one function to the existing frozen `flowdex` bootstrap:

```js
const verification = await flowdex.verify(
  ["git diff --check", "just test -p codex-flowdex"],
  { workdir: "codex-rs", timeoutMs: 120_000 },
);
```

### `flowdex.verify(commands, options?)`

`commands` is a non-empty array of non-empty command strings.

The only initial options are:

- `workdir`: optional repository-relative working directory. Default to the workflow turn's current environment directory.
- `timeoutMs`: optional timeout applied independently to each command. Omission uses the existing shell-command default.

Do not add environment overrides, login-shell selection, sandbox overrides, approval fields, parallelism, continue-on-failure, or per-command option objects in this batch. Verification inherits the same shell, environment, permissions, sandbox, and approval policy as an ordinary Codex shell command.

Return:

```js
{
  passed: true,
  commands: [
    { command: "git diff --check", exitCode: 0, durationMs: 21 },
    { command: "just test -p codex-flowdex", exitCode: 0, durationMs: 904 },
  ],
}
```

or, on the first failed command:

```js
{
  passed: false,
  commands: [
    { command: "git diff --check", exitCode: 1, durationMs: 18, output: "..." },
  ],
}
```

Each executed command result contains:

- `command`: the original command string.
- `exitCode`: the process exit code.
- `durationMs`: integer execution duration in milliseconds.
- `timedOut: true` only when the command timed out; omit it otherwise.
- `output`: bounded aggregated stdout/stderr only for a failed or timed-out command; omit it for passing commands and when empty.

Use the existing shell output bound and aggregated ordering. Do not return separate unbounded stdout and stderr fields.

Non-zero exit and timeout are verification failures, not JavaScript exceptions. Invalid arguments, an invalid working directory, denied execution, or an internal execution error remain tool errors because the requested verification could not be run correctly.

## Silent Behavior

"Silent" means no additional model turn and no verification output appended to the parent model context when commands pass.

The normal command lifecycle events should still be emitted so the user can see what the backend is doing. A passing result returns only compact metadata to JavaScript. A failure returns bounded output to JavaScript, which can be routed directly to an agent:

```js
const result = await flowdex.verify(commands);
if (!result.passed) {
  await flowdex.sendMessage(agentId, JSON.stringify(result), { delivery: "turn" });
}
```

Do not automatically choose an agent or start a repair round in Rust.

## Rust Integration Boundaries

### Private Flowdex tool

Back `flowdex.verify` with one hidden nested tool, following the Batch 002 visibility pattern:

- available only in started Flowdex workflows;
- not model-visible;
- unavailable to ordinary `functions.exec`;
- does not make `start_flowdex_workflow` recursively available.

Register it only when the current tool plan has a normal command-execution capability. It must not bypass an unavailable or disabled shell/exec surface.

### Shared shell execution

Reuse the existing shell-command argument preparation and the `ToolOrchestrator` / `ShellRuntime` execution path. Preserve:

- current environment and shell selection;
- relative working-directory resolution;
- approval and sandbox enforcement;
- hooks and command lifecycle events;
- implicit permission behavior;
- cancellation;
- existing output caps and timeout handling.

The current shell handler formats its structured `ExecToolCallOutput` into model-facing text. Make the smallest internal refactor needed for the Flowdex handler to receive the structured outcome before formatting. Keep the existing shell tool's public behavior unchanged. Do not parse the shell tool's human-readable text and do not call the low-level process executor in a way that bypasses orchestration, approvals, hooks, or events.

Deserialize the model-authored `workdir` as a regular string at the tool boundary, then immediately pass it through the existing environment-aware resolver so internal execution continues to use Codex's typed path representation. Do not add a new path wrapper or URI surface for this local workflow tool.

Keep verification orchestration in `codex-core`, where session and turn context exist. Keep only the JavaScript wrapper and context-independent bootstrap code in `codex-flowdex`; that crate must remain independent of `codex-core`.

### Cancellation

Use the nested invocation's existing cancellation token for every command. Cancelling or terminating the workflow must cancel the active command and prevent later commands from starting. Do not detach command tasks from the V8 cell lifecycle.

## Work Orders

### Task 1: Verification execution and workflow bridge

Use one `implementation_worker`.

Scope:

- `codex-rs/core/src/tools/handlers/shell.rs` and its focused submodules only for the minimal structured-output reuse
- a focused verification module under `codex-rs/core/src/tools/flowdex/`
- `codex-rs/core/src/tools/flowdex.rs`
- `codex-rs/core/src/tools/spec_plan.rs`
- `codex-rs/flowdex/src/`
- Flowdex-focused tests

Instructions:

1. Add the `flowdex.verify` bootstrap wrapper with the exact contract above.
2. Add one hidden Flowdex verification handler and include its spec only in the saved-workflow nested tool set.
3. Reuse the existing shell execution pipeline and expose the structured outcome through the smallest internal refactor.
4. Resolve `workdir` exactly as the existing shell command does, relative to the current turn environment directory.
5. Execute commands sequentially and stop after the first non-zero exit or timeout.
6. Omit passing output, bound failing output using the existing command cap, and serialize the specified result shape.
7. Ensure cancellation stops the active command and the remaining sequence.
8. Preserve the existing shell tool behavior and Batch 002 Flowdex tool visibility.
9. Commit the implementation with a brief summary before returning it to the orchestrator.

Acceptance:

- A saved workflow can run one or more verification commands and branch on `passed`.
- Passing commands do not cause an intermediate model turn or inject output into parent context.
- Failure output can be passed directly to `flowdex.sendMessage` for a repair turn.
- Sandbox, approval, hooks, environment selection, events, timeout, cancellation, and output caps match normal Codex shell execution.
- The verifier is unavailable when command execution is unavailable.
- No task, phase, repair-budget, or reviewer abstraction is added.

Verification:

- Add one Flowdex integration test that exercises an ordered pass and a non-zero failure and asserts the JavaScript result shape.
- Assert that a command after the first failure did not run within the same integration when this can be done without adding brittle platform-specific machinery.
- Add a narrow existing-shell regression assertion only if the structured-output refactor changes code not already covered by current shell tests.
- Avoid separate schema-only tests for statically defined fields.

### Task 2: Verification documentation

Use one `implementation_worker_fast` after Task 1 is committed.

Scope:

- `flowdex-plan/flowdex-documentation/`
- this Plan 003 file copied unchanged into the implementation worktree

Instructions:

1. Document the actual `flowdex.verify` signature, options, result shape, stop-on-first-failure behavior, and one agent-repair composition example.
2. Explain precisely what silent verification does and does not suppress.
3. Record any implementation detail the next planner needs if it differs from this plan.
4. Keep the current limitations accurate.
5. Commit the documentation with a brief summary.

Acceptance:

- A workflow author can use verification and route a failure without reading Rust source.
- Documentation claims only behavior implemented in this batch.

## Orchestrator Integration and Review

Apply the task commits in order and keep the worktree clean between them. Workers must be told they are not alone in the codebase and must preserve Batch 001/002 work and unrelated user changes.

After the full verification path works:

1. Run `just fmt` from `codex-rs`.
2. Run focused `codex-flowdex`, Flowdex integration, and changed shell-handler tests.
3. Run `just fix -p codex-flowdex` and `just fix -p codex-core` as applicable.
4. Use one final `gpt-5.6-luna` reviewer at `xhigh`. Focus it on command-policy bypasses, context leakage, output bounds, cancellation, tool availability, and accidental changes to the normal shell tool.
5. Fix material findings in small commits without expanding the feature.
6. Compact context at the batch boundary and message the planning task with commits, verification, the actual API/result shape, implementation notes, reviewer findings/fixes, and constraints relevant to Plan 004.

Do not run the complete workspace suite unless a focused failure makes it necessary or the user approves it.

## Non-Goals for Batch 003

- Task, phase, run, dependency, or dynamic-queue schemas.
- Automatic repair loops or verification round budgets.
- Reviews or structured findings.
- Worktree creation, task commits, integration, or attribution.
- SQLite runtime state.
- Context chunks, agent reuse, compaction, or handoffs.
- Model-facing workflow suspension, user-steering wake behavior, or human boundaries.
- Reasoning-summary progress events.
- Tool profiles, configuration, installer behavior, or AST-grep rules.
