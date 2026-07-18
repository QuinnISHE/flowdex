# Flowdex Implementation Plan 009: Task Worktrees and Commit Attribution

## Outcome

A saved Flowdex workflow can create an implementation task, run any agent against that task in an isolated Git worktree, run the task's declared verification there, and atomically integrate the task's committed changes into the workflow's original worktree.

Flowdex records which task agent produced each source commit and which commit resulted after integration. This supplies the attribution seam later review routing needs without adding reviewers, phases, or a scheduler now.

This is still a role-neutral workflow primitive. A task agent may implement, inspect, repair, or review according to its instructions. The runtime does not encode worker or reviewer roles and does not impose a repair/review loop.

## Why this is the next slice

Batches 002, 003, and 008 established generic agents, silent verification, and exact submission-owned agent reuse. The next requested capability that changes the data shape is isolated task execution:

1. Turn a task declaration into a durable task record and detached worktree based on the current integrated commit.
2. Turn an agent specification into an exact agent turn whose current directory is that task worktree.
3. Turn commits appended by that exact turn into task/agent/model attribution records.
4. Turn the task's stored verification commands into a verification result tied to the verified task `HEAD`.
5. Turn the task's linear source commits into an atomic cherry-pick on the workflow worktree and record the source-to-integrated commit mapping.

This closes one complete task lifecycle. Dependency scheduling, dynamic queues, phases, review reports, context packs, and boundary policies can later compose this lifecycle instead of predicting its Git or persistence behavior.

## JavaScript contract

Add one saved-workflow task constructor:

```js
const task = await flowdex.createTask({
  name: "update-parser",
  instructions: "Update the parser for the new record layout.",
  readScope: ["codex-rs/parser/**"],
  writeScope: ["codex-rs/parser/**"],
  verification: ["cargo test -p codex-parser"],
});
```

`name` and `instructions` are required and non-empty after trimming. `readScope`, `writeScope`, and `verification` are optional arrays of non-empty strings. Scopes are declarations included in agent instructions and stored for later reporting; they are not access controls. Reject unknown fields and invalid containers. Do not add dependencies, phase IDs, review settings, repair budgets, boundary settings, context packs, priorities, retries, or timeouts in this batch.

`createTask` resolves to a frozen JavaScript handle with exactly these public members:

```js
task.id
await task.runAgent(agentSpec)
await task.verify()
await task.integrate()
```

The handle is implemented by the Flowdex bootstrap around an opaque task ID. Do not expose the worktree path or database path to workflow JavaScript.

### `task.runAgent(agentSpec)`

`agentSpec` uses the existing Flowdex agent fields:

```js
const result = await task.runAgent({
  name: "parser-implementer",
  instructions: "Implement the task and commit any changes with a brief summary.",
  profile: "implementation_worker",
  // model and reasoningEffort remain available as existing alternatives.
});
```

Require `name`, `instructions`, and at least one existing selector among `profile`, `model`, or `reasoningEffort`. Apply the same profile/model/reasoning resolution and collaboration availability limits as `flowdex.spawnAgent`.

The runtime prepends the task requirements and advisory scopes to the supplied agent instructions. It also tells the agent that any modifications must be committed before it finishes, with a brief useful commit summary. If the agent makes no changes, no commit is required.

The method creates a fresh StatusOnly child in the task worktree and waits for that exact initial operation to reach terminal state. Return the existing bounded Flowdex agent result vocabulary unchanged:

```js
{ agentId, status: "completed", message? }
{ agentId, status: "errored", message? }
{ agentId, status: "shutdown" }
{ agentId, status: "notFound" }
```

Do not add commit data or task state to this result. The task database owns that information.

The returned agent remains a normal reusable Flowdex agent. `flowdex.resumeAgent` may run later turns on it with `keep`, `compact`, or `handoff`. For a task-associated agent, each resumed turn must use the same task operation serialization and commit-attribution path. A handoff replacement remains associated with the task and uses the same worktree. Queue delivery through `flowdex.sendMessage` remains allowed; reject `delivery: "turn"` for a task-associated agent and direct callers to `resumeAgent`, because the combined resume operation is what provides exact completion and attribution.

Only one agent turn may actively own a task worktree at a time. This does not limit concurrency across different tasks.

### `task.verify()`

Run the task's stored verification commands sequentially in its worktree through the existing Batch 003 verification path. Preserve shell policy, hooks, approvals, cancellation, output bounds, stop-on-first-failure behavior, and the existing result shape.

If the task has no configured verification commands, reject `task.verify()` with a clear error; integration may proceed without verification in that case. On a passing result, store the exact task `HEAD` that passed. A later commit invalidates that verification naturally because the stored hash no longer equals the current task `HEAD`. Failed verification is an ordinary result and does not wake or select an agent; workflow JavaScript decides what to do next.

### `task.integrate()`

Integration succeeds only when:

- the task worktree has no uncommitted changes;
- every task commit is a linear descendant of the task's recorded base commit, with no merge commits or history rewrites;
- every commit is attributable to a completed task-agent operation;
- when verification commands were configured, the current task `HEAD` is the same commit that most recently passed verification; and
- the workflow's integration worktree is clean and has no Git operation already in progress.

Cherry-pick all source commits as one Git sequence onto the workflow's original worktree. If any commit conflicts, abort the whole sequence back to the pre-integration `HEAD`, preserve the task worktree and records, and return a bounded tool error. Unless workflow JavaScript catches that error, the workflow cell fails and wakes the orchestrator for judgment. Do not guess a conflict resolution or choose an agent in Rust.

After success, record one source-to-integrated commit mapping per commit, then remove only the exact runtime-created detached worktree and mark the task integrated. The source worktree is no longer needed because its commits now exist on the integration branch and their mapping is durable.

Return only the accepted commit information:

```js
{
  taskId,
  commits: [
    {
      sourceCommit,
      integratedCommit,
      agentId,
      model,
      summary,
    },
  ],
}
```

An unchanged task may integrate successfully with `commits: []`. Do not add success/status booleans, conflict variants, cleanup options, branch names, or force controls.

## Git worktree behavior

- Require a trusted Git repository. Use the active trusted repository identity already established for repository Flowdex configuration; do not create worktrees for an untrusted or non-Git session.
- Treat the current Git worktree containing the workflow turn as the integration worktree, including when it is itself a linked worktree.
- Create each task with `git worktree add --detach` from the integration worktree's current `HEAD`. Detached task worktrees avoid temporary branch naming and cleanup.
- Put runtime-owned task worktrees under `$CODEX_HOME/flowdex/worktrees/`, separated by repository, run, and task IDs. Generate these components in Rust; no model-provided path may select or escape the root.
- Invoke Git with exact argument arrays rather than a shell command. Bound captured output and include only useful failure details in errors.
- Keep scopes advisory. Do not reject a commit because it changed a path outside a declared scope.
- Serialize integration into a given workflow worktree. Tasks may execute concurrently in separate worktrees, but cherry-picks must not race.
- Before removing a task worktree, resolve and verify that the recorded native path is the runtime-created directory under the configured Flowdex worktree root. On any uncertain or failed state, preserve it for recovery rather than deleting broadly.

For new internal path-bearing types, follow the repository path guidance: use `AbsolutePathBuf` for resolved host-local repository/worktree roots and ordinary `String` only at model-generated tool boundaries. Store native local paths in SQLite; do not introduce `PathUri` or path URIs into persistence.

## Task operation ownership and attribution

Add a per-task operation gate alongside the per-thread submitted-operation gate from Batch 008. It covers `task.runAgent` and any `resumeAgent` turn for a task-associated thread through terminal completion. This prevents two agents from editing the same worktree concurrently and gives each appended commit one unambiguous operation owner.

At the start of an owned operation, record the task `HEAD`. When that exact agent operation reaches terminal state:

1. Read the new task `HEAD` and require the prior recorded `HEAD` to remain its ancestor.
2. Enumerate newly appended commits in order.
3. Reject merge commits and rewritten/non-linear history.
4. Record each commit with the task ID, agent thread ID, resolved model, and commit message summary.
5. Release the task operation gate.

Uncommitted files do not change the agent's terminal result. They make `task.integrate()` fail clearly, leaving the worktree available so the workflow can resume the agent and ask it to commit or repair the state.

Do not infer attribution from Git author names, timestamps, or later blame. Later review routing will use Git blame on integrated commits and then join the integrated commit hash to these stored mappings.

## Minimal durable state

Add a small Flowdex-owned SQLite store in `codex-flowdex`; do not put Flowdex task tables into Codex's general state database. Open one database per trusted repository under `$CODEX_HOME/flowdex/` using a stable repository key, and verify the stored repository identity when opening it.

Use local migrations and store only the data this lifecycle needs:

- runs: current CellId/run ID, parent Codex thread, workflow path, repository identity, integration worktree, and creation time;
- tasks: task ID, run ID, declaration fields, base commit, detached worktree path, lifecycle state, and last verified commit;
- task agents/operations: task ID, agent thread, resolved model, operation ID, start commit, terminal state, and order;
- task commits: source commit, eventual integrated commit, owning task operation/agent, order, and commit summary.

Create or upsert the run lazily on the first `createTask` call using the current code-mode CellId as the run identifier. The Flowdex bootstrap already owns `workflowPath`; pass it to the hidden tool as runtime-owned bootstrap data rather than accepting a separate caller field.

Use SQLite transactions for task creation metadata, operation completion/commit recording, verification hash updates, and final integration mapping. Do not add phase, dependency, event, message, review, context, rule-candidate, retry, or generic metadata tables in this batch.

The database is durable evidence and future scheduler state, but this batch does not add process-restart workflow resumption or an orphan-cleanup command. A failed or interrupted task remains preserved rather than being silently discarded.

## Code boundaries

Keep repository-independent task declarations, SQLite migrations/storage, Git task lifecycle, and commit mapping in `codex-flowdex`. It may use focused existing Git/path utilities where they fit, but should not depend on `codex-core`.

Keep session/turn concerns in `codex-core`:

- resolve the trusted repository and integration worktree from the current session;
- extract the current code-mode CellId;
- construct a task child config whose `cwd` is the task worktree while preserving the existing profile/model/reasoning, sandbox, approval, hook, environment, and collaboration behavior;
- bind task operations to AgentControl's exact submitted-operation results;
- reuse the existing verification executor with the task worktree as its trusted runtime-owned working directory; and
- register hidden Flowdex-only task tools and bootstrap methods.

Do not expose task internals to the parent model, ordinary `functions.exec`, general code-mode calls, or recursive workflow-start tools. Do not add a feature flag, compatibility shim, placeholder scheduler, generic event bus, or a second agent lifecycle.

## Implementation tasks

The orchestrator should copy this plan unchanged into its worktree before implementation. Workers are not alone in the codebase: each must preserve prior Flowdex behavior and other workers' commits, commit only its owned changes, and use a brief descriptive commit message.

### Task 1: Flowdex task store and Git lifecycle

Use `implementation_worker` for `codex-rs/flowdex/**` plus the minimal workspace dependency/build metadata needed by that crate.

- Add the per-repository SQLite open/migration path and the minimal run/task/operation/commit records above.
- Add detached worktree creation, task `HEAD` inspection, linear commit enumeration, verification-hash storage, atomic cherry-pick integration, mapping persistence, and safe successful cleanup.
- Keep public APIs data-oriented and independent of core session types. Accept resolved host-local paths and opaque string IDs from core.
- Add focused crate tests for one complete worktree/commit/integration path and one conflict/rollback preservation path. Do not build a broad Git test matrix.

Commit this task before Task 2 begins.

### Task 2: Hidden workflow task bridge

Use `implementation_worker` for the task-related `codex-rs/core/**` changes and `codex-rs/flowdex/src/lib.rs` bootstrap wrapper changes.

- Add `createTask`, the frozen task handle, `runAgent`, `verify`, and `integrate` with the exact contracts above.
- Reuse existing agent selector resolution, StatusOnly delivery, exact submitted-operation ownership, output bounds, and collaboration/depth gates.
- Associate task agents and handoff replacements with the task; make `resumeAgent` participate in task operation serialization and attribution without changing its JavaScript result.
- Reject trigger-turn `sendMessage` for task-associated agents while preserving queue delivery.
- Reuse the Batch 003 verification execution seam with a runtime-owned task cwd; do not copy shell execution logic.
- Add one cohesive Flowdex integration test that creates a task, runs an agent which commits, verifies, integrates, and proves source/integrated commit attribution and cleanup. Include the important dirty-worktree or stale-verification rejection in that same path if it is inexpensive; do not add a test for every schema field.

Commit this task before documentation.

### Task 3: Source-of-truth documentation

Use `implementation_worker_fast` for `flowdex-plan/flowdex-documentation/**` only.

- Add a concise task-worktree usage page showing the four task-handle operations and an explicit repair flow written in ordinary JavaScript.
- Document that roles and loops remain workflow-defined, scopes are advisory, agents must commit modifications, verification is tied to task `HEAD`, integration is atomic, and conflicts preserve the task for orchestrator judgment.
- Document the per-repository database/worktree locations conceptually without exposing them as workflow API.
- Link the page from the existing Flowdex documentation index.

Commit the documentation separately.

## Focused verification and review

After the cohesive path is implemented:

1. Run formatting and scoped fixes for only the affected crates.
2. Run `cargo test -p codex-flowdex`.
3. Run the focused Flowdex core integration test and the existing Flowdex integration group. On Windows, retain the established larger Rust test-thread stack when required by the existing harness.
4. Run focused existing AgentControl and verification tests only where their shared seams changed.
5. Run `cargo check -p codex-flowdex -p codex-core` and `git diff --check`.

Do not run the full workspace suite unless a focused result points to a cross-workspace issue.

Once the complete task lifecycle passes, use one `gpt-5.6-luna` reviewer at `xhigh` for a cohesive review of:

- worktree containment and cleanup safety;
- atomic conflict rollback and preservation;
- exact task-operation ownership and commit attribution;
- child cwd/sandbox/approval/hook behavior;
- verification-to-`HEAD` binding;
- hidden visibility and StatusOnly isolation; and
- SQLite transaction consistency with Git state.

Fix actionable findings, rerun only affected focused checks, and do not dispatch a second reviewer.

## Explicitly deferred

- Run/phase scheduling, dependencies, dynamic task queues, phase inheritance, and phase/run verification.
- Review report tools, line-range blame routing, review budgets, and human/orchestrator boundary choices.
- Context packs, stale-fragment refresh, tool profiles, explicit workflow signals, and generic waits.
- Process-restart workflow resumption, task cancellation, orphan cleanup UI/commands, and manual task administration.
- AST-grep candidate promotion, remaining Flowdex configuration, and Windows app backend installation.

Plan 010 should use the implemented task lifecycle as observed. Do not design or add its scheduler API in this batch.
