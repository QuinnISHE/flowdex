# Flowdex Batch 005: Event-driven workflow waits

The orchestrator model can suspend on a yielded workflow run with
`wait_flowdex_workflow`. The wait observes the existing code-mode cell and
returns when the workflow yields, reaches a terminal state, or relevant turn
input arrives. It has no polling timeout.

## API

Call the direct model tool with exactly one field:

```json
{ "run_id": "cell-1" }
```

`run_id` is the `runId` returned by `start_flowdex_workflow`. Lifecycle events
reuse the start result shape and bounds:

```json
{ "runId": "cell-1", "status": "yielded", "output": "..." }
```

`status` is one of `yielded`, `completed`, `failed`, or `terminated`. A
boundary result is also possible:

```json
{ "runId": "run-1", "status": "boundary",
  "scope": { "kind": "task", "name": "parser" },
  "target": "orchestrator", "reason": "review exhausted" }
```
`output` is bounded as in code mode. `error` is optional and appears only for a
JavaScript failure.

User steering returns:

```json
{ "runId": "cell-1", "status": "steered" }
```

A parent-directed agent message that requests a turn wake returns:

```json
{ "runId": "cell-1", "status": "message" }
```

Neither result includes the pending input. Steering and message activity remain
owned by the input queue and are delivered once through Codex's normal next-turn
input path. They do not cancel the live workflow, drain its input, or consume
its buffered output; a later wait can continue observing the same run.

Only existing trigger-turn mailbox messages wake the wait. Queue-only mailbox
activity remains pending and does not wake the orchestrator.

The tool is invoked by the orchestrator model. It is not available in saved
JavaScript's nested `tools` object, `functions.exec`, or the general code-mode
nested tool set. Workflows use the existing `yield_control()` helper when they
explicitly need to wake the orchestrator.

For a boundary, call the direct model-only
`continue_flowdex_workflow({ "run_id": "run-1" })` tool. It accepts exactly
`run_id`, resumes that run's current boundary once, and rejects missing, stale,
terminal, or already-consumed boundaries. Steering wakes do not consume it.

## Current limits

This slice provides no scheduler, durable event history, named signals, human
checkpoints, or generic wait selector.
