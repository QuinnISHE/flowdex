# Flowdex Batch 022: Durable Workflow Pause and Resume

## Goal

Allow a Flowdex scheduler run to pause intentionally or recover after interruption or failure without restarting completed work.

The resumed run keeps its existing run ID, integrated commits, task worktrees, context fragments, review findings and counters, signals, dynamic tasks, sealed phases, and pending boundaries.

## Public tools

Add two direct-model-only tools:

```text
pause_flowdex_workflow({ run_id })
  -> { runId, status: "paused" }

resume_flowdex_workflow({ run_id })
  -> { runId, status: "resumed" }
```

`pause_flowdex_workflow` requests a cooperative pause and resolves after the scheduler reaches a stable checkpoint. It does not interrupt an active agent, verification command, review, or integration operation.

`resume_flowdex_workflow` resumes a paused, interrupted, or failed scheduler run. It rejects completed runs and runs that already have an active scheduler.

`wait_flowdex_workflow` remains the event-driven observer and additionally returns:

```text
{ runId, status: "paused" }
```

A resumed run reports scheduler completion or failure through `wait_flowdex_workflow` even when its original saved-workflow V8 cell no longer exists. Normal uninterrupted workflows retain their exact JavaScript output behavior.

## Durable reconstruction

Persist every field needed to reconstruct `WorkflowDefinition`. In particular, add the currently missing task `context` list to `workflow_tasks`.

Add one store operation that loads a complete validated workflow definition from:

- `runs`
- `workflow_agents`
- ordered `workflow_phases`
- ordered `workflow_tasks`
- `context_packs`

Repository identity and the integration worktree remain authoritative. Resume must open the store for the current trusted repository and reject a run from another repository.

Store the latest bounded run error for inspection and clear it when resuming. Do not add a second event log or duplicate task metadata.

## Scheduler checkpoints

Add `Paused` to durable and live run states.

The scheduler checks for a pause:

- before entering a new phase;
- before launching a ready task batch;
- after an in-flight task batch has drained;
- before phase or run verification and review.

Already-running work reaches its existing durable transition before pause completes.

On resume:

- completed phases are skipped;
- integrated tasks are skipped;
- queued and ready tasks retain their state;
- transient `running` or `attributing` tasks return to `ready`;
- implemented tasks resume at verification/review/integration;
- verified tasks resume at review/integration;
- failed task worktrees and commit attribution are retained.

Record an `implemented` task checkpoint immediately after the implementation agent operation completes successfully. A later verification, review, sandbox, or integration failure must not erase that checkpoint.

If an implemented task needs model repair and its previous agent thread is unavailable, spawn a replacement task agent in the existing task worktree with the original requirements and bounded failure details. Do not create another worktree.

## Interruption behavior

Scheduler cancellation no longer converts an otherwise recoverable run into a terminal failed graph. It records `paused` when possible and leaves transient durable states recoverable when the process exits before that write.

After a process restart, `resume_flowdex_workflow` reconstructs a new live controller from SQLite and the current tool invocation. The original JavaScript stack and custom `flowdex.output(...)` value are not recreated; the resumed scheduler returns its standard terminal run result.

## Progress and UI

Emit existing live-only reasoning summaries for pause requested, paused, resumed, and recovery of an incomplete task. Do not add them to model history.

Resumed task and review agents continue through the existing app-server lifecycle event path.

## Focused verification

- Store round-trip reconstructs agents, tool profiles, phases, dynamic tasks, task context lists, reviews, boundaries, and context-pack declarations.
- Pausing an open or dependency-blocked phase reaches `paused` without losing queued state.
- Resume skips an integrated task and completes remaining work under the same run ID.
- A task that failed after implementation retries verification/integration without creating a second task worktree.
- Simulated controller loss reconstructs from SQLite and reaches a terminal scheduler result.
- Existing uninterrupted saved-workflow output behavior remains unchanged.

Run the focused Flowdex store/core tests and crate checks only.

## Non-goals

- Resurrecting a terminated V8 stack or arbitrary JavaScript local variables.
- Pausing in the middle of an agent turn, shell command, cherry-pick, or SQLite transaction.
- Automatic retry loops without an explicit resume request.
- Compatibility shims, feature flags, aliases, or a second scheduler.
