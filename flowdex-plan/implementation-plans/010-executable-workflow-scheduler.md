# Flowdex Implementation Plan 010: Executable Workflow Scheduler

## Outcome

A saved Flowdex JavaScript file can start one durable run containing named agents, sequential phases, and dependency-aware tasks. Flowdex schedules independent tasks concurrently, executes each task through the accepted Batch 009 worktree lifecycle, verifies and integrates their commits, advances phases, and completes without waking the orchestrator between operations.

This batch also corrects progress ownership first. Saved workflows and models must no longer call `flowdex.progress(...)`; real scheduler transitions emit live reasoning-summary events automatically without adding them to model context or persisted conversation history.

This is the first final workflow-language slice, not a schema preview. Every field and method introduced here must execute in this batch. Reviews, repair budgets, context packs, tool profiles, and boundaries remain later extensions and must not be accepted as inert fields.

## Why this is the next slice

Batches 001-009 now provide the required execution substrate: saved native-V8 workflows, role-neutral agents, exact operation waits, task worktrees, verification, commit attribution, atomic integration, event-driven cell waiting, and user-steer wakeups. What is missing is the layer that turns those capabilities into a workflow rather than requiring authors to hand-code individual agent and task calls.

The scheduler should consume the Batch 009 lifecycle directly. Do not replace its worktree, verification, operation ownership, or integration rules, and do not add a second agent runtime, command executor, event bus, database, or capacity system.

## Progress correction comes first

Make this a small committed task before scheduler work begins:

- Remove `flowdex.progress(...)` from the frozen JavaScript bootstrap.
- Remove the hidden `flowdex_progress` tool specification, handler registration, and tests that treat it as callable.
- Correct the progress documentation so it no longer teaches explicit progress calls.
- Preserve the existing live-only reasoning-summary delivery seam and its guarantees: ordinary app-server reasoning item lifecycle, unique item IDs, no rollout/conversation/model-history persistence, no next-request injection, and no workflow-result output.
- Keep that seam internal to Codex core so the scheduler can call it later in this same batch.

Do not replace it with another public tool or leave an unused alternate progress API. Scheduler transitions below become its only Flowdex caller.

## JavaScript contract

The durable high-level entry point is:

```js
const run = await flowdex.startRun({
  name: "update-record-layout",
  agents: {
    implementer: { profile: "implementation_worker" },
    fastFix: {
      model: "gpt-5.6-luna",
      reasoningEffort: "high",
    },
  },
  verification: ["git diff --check"],
  phases: [
    {
      name: "implementation",
      instructions: "Implement the record-layout changes and commit each task with a brief summary.",
      verification: ["cargo test -p record-layout"],
      tasks: [
        {
          name: "parser",
          agent: "implementer",
          instructions: "Update the parser.",
          dependencies: [],
          readScope: ["src/parser/**"],
          writeScope: ["src/parser/**"],
          verification: ["cargo test -p parser"],
        },
        {
          name: "serializer",
          agent: "implementer",
          instructions: "Update the serializer.",
          dependencies: [],
          readScope: ["src/serializer/**"],
          writeScope: ["src/serializer/**"],
          verification: ["cargo test -p serializer"],
        },
        {
          name: "round-trip",
          agent: "fastFix",
          instructions: "Add round-trip coverage for the integrated parser and serializer changes.",
          dependencies: ["parser", "serializer"],
          readScope: ["src/**"],
          writeScope: ["tests/**"],
        },
      ],
    },
  ],
});

const result = await run.wait();
```

`flowdex.startRun(definition)` starts scheduler execution and resolves to a frozen handle with exactly:

```js
{
  id,
  queueTask,
  sealPhase,
  wait,
}
```

- `id` is the existing Flowdex code-mode `CellId`; one saved workflow cell owns at most one run.
- `wait()` waits on scheduler state without polling and resolves to `{ runId, status: "completed" }`. A failed run rejects with the existing bounded tool error so the enclosing JavaScript cell and `wait_flowdex_workflow` report failure normally.
- `queueTask(phaseName, taskDefinition)` validates and appends one task to an open phase, then resolves to `{ taskId }` without interrupting unrelated running tasks.
- `sealPhase(phaseName)` prevents further additions to that phase and resolves to JavaScript `undefined`.

All wrapper objects are strict plain objects. Reject arrays, null, primitives, unknown keys, empty-after-trim strings, unknown agents, duplicate names, missing dependencies, dependency cycles, and invalid option arrays. Do not silently drop fields.

### Run definition

Accept only:

- `name`: required non-empty string.
- `agents`: required non-empty plain object keyed by reusable agent name.
- `phases`: required non-empty array.
- `verification`: optional array of non-empty command strings, run once after the final phase against the integrated workflow worktree.

Each agent value accepts only `profile`, `model`, and `reasoningEffort`, with at least one selector present. These retain the normal `.codex/agents` resolution and override behavior. Agents remain role-neutral.

### Phase definition

Accept only:

- `name`: required non-empty string, unique within the run.
- `instructions`: required non-empty string inherited by every task in the phase.
- `tasks`: required array of task definitions; it may be empty only when `open` is true.
- `open`: optional boolean, default `false`. A closed phase is sealed as soon as the definition is accepted. An open phase waits after its current queue drains until `sealPhase` is called.
- `verification`: optional array of non-empty command strings, run against the integrated workflow worktree after the sealed phase has integrated every task.

Phases execute in declaration order. A future open phase may accept queued tasks before it becomes active.

### Task definition

Accept only:

- `name`: required non-empty string, unique within its phase and used by dependencies.
- `agent`: required name from the run's agent map.
- `instructions`: required non-empty string.
- `dependencies`: optional array of task names in the same phase, default `[]`.
- `readScope`, `writeScope`, and `verification`: the accepted Batch 009 arrays of non-empty strings.

The scheduler prepends the phase instructions to the task instructions once, then uses that combined instruction for both the durable task declaration and the fresh StatusOnly task agent. Do not add worker/reviewer/explorer roles.

Initial definitions validate the complete per-phase dependency graph atomically before creating scheduler state. A dynamically queued task may depend only on tasks already present in that phase. Rejecting a dynamic addition must not fail or pause the existing run.

## Scheduling behavior

- Run one phase at a time.
- Within the active phase, a task is ready only when all dependencies are integrated.
- Dispatch every ready task concurrently by default, bounded by Codex's existing shared agent capacity. Do not add `maxParallel`, a Flowdex semaphore, or a parallel-task wrapper.
- Capacity exhaustion leaves excess ready tasks queued and retries them after an active task reaches a terminal state; it is not a task failure and must not use a timer.
- Dependencies are the source of semantic ordering.
- Advisory scopes remain non-enforcing. Avoid an obvious concurrent write/write collision when normalized write-scope roots are equal or one is an ancestor of the other; serialize that pair in declaration order. Do not attempt a general glob-intersection engine. Unexpected overlap remains isolated by task worktrees and is handled by Batch 009 integration rules.
- Dynamic tasks append to their phase's declaration order.
- Integrate completed tasks in stable declaration order. Different agents may finish out of order, but Git integration must remain deterministic.
- Mark a dependency satisfied only after its task integrates successfully.
- A task agent error, shutdown, missing commit attribution, failed task verification, or integration failure fails the run with bounded context and preserves the Batch 009 task/worktree evidence.
- A task with no verification commands skips task verification and may integrate, matching Batch 009 behavior.
- After every task in a sealed phase integrates, run phase verification if configured, then advance to the next phase.
- After the last phase, run run-level verification if configured and then complete.

Task, phase, and run verification remain separate operations. This batch does not repair a failure or spend a review/repair budget; it reports the first failed stage and terminates the run.

## Dynamic task control

The JavaScript handle methods above are real scheduler commands, not local bookkeeping. Register them as hidden saved-workflow tools only.

Also expose direct-model-only `queue_flowdex_task` and `seal_flowdex_phase` tools backed by the same validation and scheduler commands. They let an already-awake orchestrator add work or close an open phase by `run_id` without stopping the active workflow. They must not appear inside ordinary `functions.exec`, general code mode, or saved workflows under different names.

Do not add a generic run mutation tool. Exact schemas are:

```text
queue_flowdex_task({ run_id, phase, task }) -> { taskId }
seal_flowdex_phase({ run_id, phase }) -> acknowledgement
```

User steering continues to wake `wait_flowdex_workflow` through the accepted Batch 005 path and does not cancel the run. Queue-only mailbox messages continue not to wake it.

## Automatic progress and app visibility

Emit progress directly from committed scheduler transitions. No model or workflow JavaScript call is involved.

Use deterministic bounded summaries:

- `Running workflow: {run}`
- `Running phase {index}/{total}: {phase}`
- `Running task: {task}`
- `Verifying task: {task}`
- `Integrating task: {task}`
- `Completed task: {task}`
- `Verifying phase {index}/{total}: {phase}`
- `Completed phase {index}/{total}: {phase}`
- `Verifying workflow: {run}`
- `Completed workflow: {run}`
- `Task {task} failed during {stage}` for a terminal task-stage failure

Parallel task starts and completions each emit their own item. Do not coalesce them through a model. Future review/boundary slices may add templates, but must use the same transition emitter.

Preserve the Batch 004 non-persistence guarantees. These summaries are live app-server reasoning items only and must be absent from rollout items, conversation history, the next model request, workflow output, and resume state.

Task agents must continue through the normal Codex spawn and status paths so the app server and GUI receive ordinary subagent lifecycle/status/graph events. `StatusOnly` suppresses only automatic completion injection into the parent model; it must not suppress app-visible agent events, final-output capture, or attribution.

## Minimal durable state

Broaden the existing per-repository Flowdex SQLite store in place; do not create another database or persistence abstraction. Because it now owns more than isolated tasks, rename the internal `TaskStore`/file once to a durable `FlowdexStore`/`store` name, with no compatibility alias.

Persist only what execution and inspection need:

- run name and state;
- ordered phases, phase instructions, open/sealed state, verification commands, and phase state;
- task phase membership, declaration order, named agent selection, dependency edges, and scheduler state;
- the existing task worktree, operation, commit, verification, and integration records unchanged in meaning.

Use explicit finite states for run/phase/task scheduling rather than a generic event log. State changes that make work ready, running, integrated, failed, or complete must be transactional with the scheduler decision they represent.

Process-restart resumption and orphan administration remain deferred. Persistence in this batch supports live scheduler coordination, dynamic additions, exact attribution, and later inspection; do not add speculative recovery fields.

## Code boundaries

- `codex-flowdex` owns strict workflow definitions, dependency validation, scheduler state transitions, SQLite schema, readiness queries, declaration order, and scope-conflict hints. It remains independent of `codex-core`.
- `codex-core` owns the live run service, hidden/direct tool handlers, normal AgentControl spawning, exact operation waits, verification execution, Git lifecycle calls, cancellation, and app-server progress delivery.
- Reuse the Batch 009 task bridge internally rather than having the scheduler call its JavaScript wrappers.
- Keep `CellId` as the run/controller identifier and the existing code-mode cell lifecycle as the outer completion seam.
- Keep the accepted low-level `createTask`, agent, messaging, resume, and verification primitives available as deliberate advanced primitives. Do not change their exact Batch 009 handle/result shapes in this batch.
- `start_flowdex_workflow`, `wait_flowdex_workflow`, `queue_flowdex_task`, and `seal_flowdex_phase` are direct-model-only. Scheduler nested tools are hidden saved-workflow tools. None are recursive workflow-start tools.

## Implementation tasks and parallelism

The orchestration agent should use gpt-5.6-sol on low and preserve small commits with brief summaries. Workers are not alone in the worktree and must not revert unrelated changes.

### Task 0: Correct progress ownership

Use `implementation_worker_fast`. Own the callable-progress removal in the Flowdex bootstrap/tool registration and the correction to `progress.md`. Preserve the internal non-persistent emitter for scheduler use. Commit this before other workers begin so later bootstrap/core work starts from the corrected surface.

### Tasks 1 and 2: Establish the shared final contract in parallel

After Task 0, run these concurrently:

1. Use `implementation_worker` for `codex-flowdex` workflow definitions, dependency validation, the in-place `FlowdexStore` rename/schema extension, and scheduler state/readiness operations. Own `codex-rs/flowdex/src/` except the bootstrap construction in `lib.rs`.
2. Use `implementation_worker` for the strict JavaScript `startRun` handle/bootstrap and wrapper validation in `codex-rs/flowdex/src/lib.rs`. Use the exact contract in this plan; do not invent fields. Limit cross-file edits to the minimum module exports needed after Task 1 lands.

These tasks share this written contract and should not independently redesign it.

### Task 3: Live scheduler and tool bridge

After Tasks 1 and 2 integrate, use one `implementation_worker` for the core run service, hidden run commands, direct dynamic-task tools, dependency-ready concurrent dispatch, stable integration ordering, verification stages, cancellation, and automatic progress emission. Reuse AgentControl capacity and the accepted task lifecycle.

### Tasks 4 and 5: Documentation and cohesive coverage in parallel

Once the cohesive path works, run these concurrently:

1. Use `implementation_worker_fast` to write `flowdex-plan/flowdex-documentation/workflows.md`, correct the index and progress documentation, and show one static parallel workflow plus one open-phase queue/seal example. Document the exact final API, failure behavior, app visibility, and current limits.
2. Use `implementation_worker` for one focused end-to-end scheduler fixture and narrow store/bootstrap tests. Do not broaden into exhaustive schema-field tests.

## Focused verification and review

Verify the cohesive behavior, not every permutation:

- `codex-flowdex` focused tests for strict definition validation, dependency cycles, dynamic-add isolation, sealing, readiness, and stable declaration order;
- one core integration workflow with two independent tasks observed running before either is released, a third task depending on both, task/phase/run verification, deterministic integration, and terminal completion;
- the same integration should confirm scheduler summaries use the live non-persistent reasoning path and do not enter the next model request;
- confirm normal app-server subagent lifecycle events still occur for workflow task agents while automatic parent-model completion delivery remains suppressed;
- confirm user steering still wakes a parent `wait_flowdex_workflow` without cancelling the live run;
- existing focused Flowdex, task lifecycle, verification, and code-mode wait regressions affected by the changed files;
- scoped formatting/fixes and `git diff --check`.

Use the established larger Windows Rust test-thread stack where the existing core integration harness requires it. Do not run the full workspace suite unless a concrete failure makes it useful.

After the full path passes, use exactly one gpt-5.6-luna reviewer on xhigh for the cohesive scheduler path. Ask it to focus on operation ownership, parallel capacity behavior, deterministic integration, dependency transitions, progress non-persistence, app-server agent visibility, cancellation, and accidental public APIs. Fix actionable findings and rerun only affected checks.

## Explicitly deferred

- verification repair loops and budgets;
- review agents, structured findings, review routing, and review-round budgets;
- task/phase/run boundaries, checkpoint approval, and configured escalation;
- context packs, stale-fragment collection, and explorer dispatch;
- Flowdex tool profiles;
- named workflow signals and generic scheduler-event selectors;
- process-restart resumption, orphan cleanup, and manual task administration;
- AST-grep candidate promotion;
- installer/configuration completion;
- a parallel-task wrapper or user-configurable Flowdex concurrency limit;
- compatibility shims, feature flags, placeholders, and alternate scheduler runtimes.
