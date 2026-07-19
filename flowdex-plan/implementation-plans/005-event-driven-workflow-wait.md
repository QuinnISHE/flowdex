# Flowdex Implementation Plan 005: Event-Driven Workflow Wait

## Outcome

The orchestrator can suspend on a yielded Flowdex run without repeatedly calling the existing timed code-mode wait tool.

Add one direct model tool:

```text
wait_flowdex_workflow({ run_id })
```

It waits until one of these material events occurs:

- The workflow explicitly calls the existing `yield_control()` helper.
- The workflow completes, fails, or is terminated.
- The user steers the active turn.
- A parent-directed agent message requests a new turn.

User steering and agent messages wake the orchestrator but do not consume the pending input and do not cancel the workflow. The next model inference receives that input through Codex's existing turn-input path.

This batch removes the polling loop from the model-facing Flowdex run lifecycle. It does not introduce the task scheduler or a general durable event system.

## Why This Is the Next Slice

Batch 001 established the code-mode cell ID as the Flowdex run ID. Batches 002 through 004 added workflow-local agents, verification, and progress, but a yielded run still leaves the orchestrator with the existing timed code-mode wait behavior.

The repository already contains the two data sources this feature needs:

- Code mode owns the cell lifecycle and already observes explicit yield, completion, failure, and termination.
- `InputQueue` exposes a `watch` subscription for `Steer` and `Mailbox` activity and can detect pending input before a waiter subscribes.

Use those sources directly. Do not add a Flowdex event bus, timer loop, detached poller, or app-server-specific notification path.

The data transformation is:

```text
{ run_id }
  -> existing CellId
  -> subscribe to the active turn's InputQueue activity
  -> start a no-deadline code-mode cell observation
  -> select the first material event
     -> cell frontier: existing bounded Flowdex runtime result
     -> pending user input: { runId, status: "steered" }
     -> wake-worthy parent mailbox input: { runId, status: "message" }
```

The pending user or mailbox item remains owned by `InputQueue`; the wait tool only observes its notification.

## Public Contract

### Model tool

`wait_flowdex_workflow`

Input has exactly one field:

- `run_id`: the `runId` returned by `start_flowdex_workflow`.

Do not add a timeout, polling interval, event list, task ID, phase ID, or metadata field in this batch.

For a workflow lifecycle event, return the same shape and bounds as the start tool:

```json
{
  "runId": "cell-1",
  "status": "yielded",
  "output": "..."
}
```

The workflow statuses remain `yielded`, `completed`, `failed`, and `terminated`. `error` remains optional and appears only for a JavaScript failure. `output` remains bounded using the existing code-mode result handling.

For user steering, return:

```json
{
  "runId": "cell-1",
  "status": "steered"
}
```

For a parent-directed mailbox message whose existing delivery semantics request a turn wake, return:

```json
{
  "runId": "cell-1",
  "status": "message"
}
```

Do not include the user or agent message in the tool result. Codex's normal pending-input path delivers it to the model once, avoiding a duplicate copy in context.

The tool is model-facing only. It must not appear in saved workflows' nested `tools` object, ordinary `functions.exec`, or the general code-mode nested tool set.

## Implementation Guidance

### No-deadline code-mode observation

Add the smallest internal code-mode observation needed to wait for an explicit yield or terminal cell event without a timer.

Keep the existing timed `wait` tool and its default behavior unchanged. The new observation should reuse the cell actor, observer ownership, buffered output, completion commit, cancellation, trace completion, and dispatch cleanup already used by code-mode waits.

A direct `UntilYield`-style observation mode is preferable to encoding an extremely large timeout. It should:

- Resume a paused cell as the existing yield wait does.
- Wake for `yield_control()` and terminal events.
- Continue through nested tool pending frontiers rather than returning merely because a nested tool is still running.
- Have no timer.
- Release observer ownership cleanly when the outer wait future is dropped because steering or turn cancellation won the race.

Keep this as an existing code-mode lifecycle capability, not a Flowdex-owned cell registry or second controller.

### Steering and mailbox observation

Subscribe through the existing `InputQueue::subscribe_activity` path before awaiting the code cell so input arriving around subscription cannot be missed.

- If a steer is already pending, return `steered` immediately.
- On `InputQueueActivity::Steer`, return `steered` without draining pending input.
- On `InputQueueActivity::Mailbox`, return `message` only when the existing queue reports a message that requests a turn wake. Queue-only mail must remain pending without waking the orchestrator.
- If an unrelated mailbox notification occurs, continue the same event-driven wait rather than returning or polling.

Normal turn interruption and session shutdown continue to use existing cancellation behavior. They are not encoded as successful Flowdex statuses.

### Result mapping and cell cleanup

Reuse or narrowly extract the result mapping already used by `start_flowdex_workflow`; do not create a second status model.

When the cell reaches a terminal event, preserve the same code-mode trace completion and dispatch cleanup performed by the ordinary wait handler. A steer or mailbox wake leaves the cell live and available for another `wait_flowdex_workflow` call.

Do not consume output on a steering or message wake. Output remains buffered with the cell and is returned by the next lifecycle observation.

## Task Breakdown

### Task 1: Implement the event-driven wait path

Use one `implementation_worker` for the cohesive Rust change.

Scope:

- `codex-rs/code-mode` and its protocol crate only where the no-deadline observer requires it.
- `codex-rs/core/src/tools/code_mode/` for shared wait lifecycle handling.
- `codex-rs/core/src/tools/flowdex.rs` or a focused Flowdex wait module.
- `codex-rs/core/src/tools/spec_plan.rs` for direct-model-only registration.
- Focused code-mode/core tests owned by this behavior.

Instructions:

- Build on the current CellId and `CodeModeService`; do not add a runtime registry.
- Build on `InputQueue`'s existing watch channel; do not add another steering channel.
- Keep the ordinary timed code-mode wait API behavior unchanged.
- Register `wait_flowdex_workflow` as direct-model-only and exclude it from all nested tool sets.
- Preserve cancellation, bounded output, terminal cleanup, and buffered output across a steering wake.
- Add no compatibility shim, feature flag, placeholder scheduler type, or speculative event schema.
- Commit the cohesive implementation with a brief summary.

Acceptance criteria:

- A yielded Flowdex run can be awaited without a timeout or repeated model calls.
- Explicit workflow yield and terminal cell states return the established bounded Flowdex result shape.
- User steering wakes the wait and remains available to the next model inference.
- A wake-worthy parent agent message wakes the wait and remains available to the next model inference.
- Queue-only mailbox messages do not spuriously wake the orchestrator.
- Steering does not terminate the workflow; a later wait can continue observing the same run ID.
- Existing non-Flowdex code-mode wait behavior is unchanged.

Focused verification:

- Add one code-mode test for no-deadline observation waking on explicit yield or completion.
- Add one cohesive Flowdex integration test that starts a workflow, enters the event-driven wait, steers the turn, confirms the steer reaches the next model request exactly once, then waits again and observes workflow completion.
- Cover wake-worthy versus queue-only mailbox delivery in the smallest existing input-queue or handler-level test if the integration test cannot express both cheaply.

Suggested commit subject: `feat(flowdex): add event-driven workflow wait`

### Task 2: Document the wait contract

Use one `implementation_worker_fast` after Task 1's actual API is stable.

Scope:

- `flowdex-plan/flowdex-documentation/waiting.md`
- `flowdex-plan/flowdex-documentation/agents.md` for a link and current-limits update.
- `flowdex-plan/flowdex-documentation/groundwork.md` only if the run lifecycle description needs a small clarification.

Document:

- The exact `wait_flowdex_workflow` input and result shapes.
- That the tool is invoked by the orchestrator model, not from saved JavaScript.
- That workflows use the existing `yield_control()` helper for an explicit orchestrator wake.
- That user steering and wake-worthy agent messages interrupt the wait without canceling the workflow or duplicating input in the tool result.
- That the wait has no polling timeout.
- The remaining limits: no scheduler, durable event history, named signals, human checkpoints, or generic wait selector yet.

Commit the documentation separately with a brief summary.

Suggested commit subject: `docs(flowdex): document event-driven workflow waits`

## Cohesive Verification and Review

After both tasks are integrated:

1. Run formatting and fixes only for the changed crates.
2. Run the focused `codex-code-mode`, `codex-flowdex`, and Flowdex core tests touched by this slice. Do not run the full workspace suite solely for this batch.
3. Confirm the existing timed code-mode wait test still passes.
4. Confirm `git diff --check` and a clean worktree.
5. Dispatch one final reviewer using `gpt-5.6-luna` at `xhigh` for the complete path. Ask it to focus on missed-input races, observer cleanup after steering wins, duplicate model-context delivery, cell output preservation, terminal cleanup, and accidental exposure of the wait tool to nested workflows.
6. Resolve actionable findings in small follow-up commits and update `waiting.md` if the implemented contract changed.

Do not add further review rounds unless the final reviewer identifies a concrete issue that was changed.

## Non-Goals for Batch 005

- Task, phase, dependency, or dynamic queue scheduling.
- SQLite run persistence or restoration after process restart.
- A generic named-signal registry or durable event log.
- Workflow-local `flowdex.wait(...)`; existing JavaScript promises and agent primitives remain the workflow-local composition mechanism.
- Human approval boundaries or request-user-input behavior.
- Automatic progress generation or progress persistence.
- Review routing, worktrees, commits, or attribution.
- Context chunks, context gathering, configuration, AST-grep promotion, installer work, or `compact_context`.
- Changing the ordinary timed code-mode wait tool's public contract.

Do not add placeholders, compatibility shims, feature flags, or future scheduler/event types for these non-goals.

## Orchestrator Handoff

Copy this plan into the implementation worktree unchanged before implementation. Keep worker ownership disjoint, require each worker to commit its own scoped change with a brief summary, and leave the worktree clean.

When Batch 005 is complete, message the planning task with:

- Commit list.
- Focused verification results.
- Final JavaScript/model tool contracts and result shapes.
- How steering and mailbox races are avoided.
- How observer cancellation and cell cleanup are handled.
- Reviewer findings and fixes.
- Facts or constraints that should shape Plan 006.

Do not design Plan 006 in the implementation task. Compact context at the batch boundary and wait for the planning task's next plan.
