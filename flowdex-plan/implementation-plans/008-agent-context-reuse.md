# Flowdex Implementation Plan 008: Agent Context Reuse

## Outcome

A saved Flowdex workflow can run another discrete turn on an existing agent while explicitly choosing whether that turn keeps the agent's history, follows a native compaction of that history, or starts in a fresh replacement thread from a structured handoff.

This is a generic agent primitive. It does not introduce worker, reviewer, explorer, task, or phase behavior.

## Why this is the next slice

Batch 002 established hidden workflow-only agent spawn, message, and event-driven completion primitives. Batch 006 established Codex's native compaction path. Those are now sufficient to implement the three requested reuse behaviors without inventing scheduler, worktree, or database APIs.

Task scheduling should wait until task worktrees, commits, and durable state can be designed as one coherent path. Reusing agents is independent of those choices and closes an existing workflow-language requirement now.

The data transformations for this slice are:

1. Resolve a Flowdex child thread and confirm that its previous turn is complete.
2. Apply the selected context transition:
   - `keep`: retain the thread and its history.
   - `compact`: run Codex's existing native compaction operation on that thread, then retain it.
   - `handoff`: ask the old thread for a bounded structured handoff, then create a fresh sibling thread with the same resolved configuration.
3. Start the requested instructions on the retained or replacement thread.
4. Wait for that specific submitted turn to finish and return its bounded terminal result to JavaScript.

No intermediate completion is delivered to the orchestrator model.

## JavaScript contract

Add one saved-workflow method:

```js
const result = await flowdex.resumeAgent(agentId, instructions, {
  contextMode: "compact",
});
```

`agentId` is an existing Flowdex child thread ID. `instructions` is required and non-empty after trimming. The optional object accepts only `contextMode`:

- `"keep"` retains the existing thread and history. This is the default.
- `"compact"` completes native compaction of the existing thread before the new instructions start.
- `"handoff"` collects a structured handoff from the existing thread, starts a fresh replacement thread, and gives the replacement both the handoff and the new instructions.

Unknown options or context modes are rejected. Do not add target selectors, custom handoff prompts, compaction strategies, timeouts, retry limits, or configuration fields.

The promise resolves only after the resumed turn reaches a terminal state. Reuse the existing Flowdex agent result vocabulary:

```js
{ agentId, status: "completed", message? }
{ agentId, status: "errored", message? }
{ agentId, status: "shutdown" }
{ agentId, status: "notFound" }
```

For `keep` and `compact`, `agentId` remains the original thread ID. For `handoff`, it is the fresh replacement thread ID. `message` follows the same bounded, optional behavior as `flowdex.waitAgent`.

This method intentionally combines dispatch and event-driven completion. A workflow can run several resumed agents concurrently with ordinary JavaScript promises, while the runtime can associate each result with the submitted turn rather than accidentally returning the target's previous terminal state.

## Context-mode behavior

### Keep

- Require the target's prior turn to be complete before starting a reuse turn.
- Send the new instructions through the existing trigger-turn inter-agent communication path.
- Subscribe before submission and wait for a status notification produced after that accepted submission before accepting a terminal status. An already-completed status from the prior turn must not satisfy the wait.
- Preserve the existing thread, history, agent path, configuration, environment, and StatusOnly completion delivery.

General queue/turn messaging remains available through `flowdex.sendMessage`; `resumeAgent` is the discrete-turn operation with completion ownership.

### Compact

- Require the same completed-target precondition as `keep`.
- Submit the existing standalone native `Op::Compact` to the child through `AgentControl`; do not ask the model to call `compact_context` and do not expose `compact_context` inside saved JavaScript.
- Wait event-first for that submitted compaction operation to finish before sending the new instructions.
- If compaction fails, is cancelled, or the thread shuts down, do not start the new instructions. Surface the existing bounded error through the Flowdex tool call.
- Preserve the established local/remote compaction implementation, hooks, persistence, initial context, cancellation, and metadata. Do not add a second compaction path or alter the direct-model `compact_context` contract.

### Handoff

- Require the existing thread's prior turn to be complete.
- Start one StatusOnly turn on the old agent using one runtime-owned prompt with this meaning: produce only a concise structured handoff containing completed work, current state, relevant files and decisions, remaining work, and verification; do not modify files or continue implementation.
- Wait for that submitted handoff turn rather than accepting its previous terminal status. If it does not complete with non-empty output, return its terminal failure and do not create a replacement.
- Bound the handoff with the existing Flowdex agent-output limit. Do not parse it into a new schema or persist a separate handoff document.
- Create a fresh sibling child under the current workflow parent at the same agent depth. Reuse the old child's resolved configuration snapshot, including model, reasoning, profile effects, tools, environment, working directory, permissions, and current StatusOnly completion delivery. The new thread starts with fresh conversation history.
- Give the replacement a simple prompt containing the handoff followed by the caller's new instructions. The caller-provided instructions remain distinct and last.
- Leave the old terminal thread intact for history and diagnostics. The returned replacement ID becomes authoritative for later workflow calls.
- Use the existing agent capacity, depth, path allocation, spawn graph, communication logging, and cancellation behavior. Do not add replacement tables or aliases in this slice.

## Implementation work

### 1. Add submission-aware child operation support

Extend the existing `AgentControl` seam only as much as needed to:

- Submit native compaction to a known child thread.
- Observe completion belonging to an operation submitted after the subscription was created.
- Return the target's resolved configuration snapshot when the handoff path needs a replacement.

Keep this support generic to child operations; do not create a Flowdex scheduler or a second agent registry. Reuse the existing status watch, thread manager submission path, execution-capacity checks, and error mapping. Coalesced status updates must still allow an operation that starts and finishes quickly to complete the waiter.

### 2. Add the hidden Flowdex reuse handler

Add one hidden Flowdex-only nested tool and expose it through the reserved `flowdex` bootstrap as `resumeAgent`.

The handler owns the three explicit context transitions above and returns exact JSON to V8. Factor the existing bounded agent status/result conversion so `waitAgent` and `resumeAgent` cannot drift, but do not otherwise redesign the Batch 002 handlers.

Register the tool only when existing collaboration tools are available. Preserve the current visibility boundary: it is available to saved Flowdex workflows, not the parent model, ordinary `functions.exec`, general code mode, or recursive workflow start/wait tools.

### 3. Preserve completion isolation

All turns created by this feature, including the handoff turn and replacement turn, use `SpawnAgentCompletionDelivery::StatusOnly` behavior. Results travel only through the workflow promise. They must not create automatic completion messages in the orchestrator context through either the legacy watcher or MultiAgentV2 terminal forwarding.

Ordinary user-visible agent lifecycle activity may remain visible through Codex's existing event path.

### 4. Update the source-of-truth documentation

Extend `flowdex-plan/flowdex-documentation/agents.md` with:

- The exact `resumeAgent` signature and result shapes.
- The three context modes and default.
- The fact that the method waits for the newly submitted turn.
- Fresh-thread and configuration-inheritance behavior for handoff.
- The distinction between `sendMessage`, `waitAgent`, and `resumeAgent`.
- Current limits: no task scheduler, worktree management, durable replacement mapping, custom handoff prompt, or tool profiles in this batch.

Keep the documentation concise and usable as source material for future Flowdex skills.

## Task decomposition

Use ordered implementation tasks with brief commits:

1. An `implementation_worker` owns the generic `AgentControl` support for native child compaction and post-submission completion observation. It should commit that cohesive support with a short summary.
2. After that commit, an `implementation_worker` owns the hidden handler, JavaScript bootstrap method, exact result mapping, and the cohesive Flowdex integration coverage. It should reuse the first task's seam rather than duplicate status logic and commit its work separately.
3. After the API is settled, an `implementation_worker_fast` updates `agents.md` and commits the documentation separately.

Workers are not alone in the codebase. They must preserve earlier Flowdex changes and unrelated user work, stay within their owned files where practical, and commit only their scoped changes.

## Focused verification

Keep verification proportional to this one path:

- Existing focused `codex-flowdex` tests.
- One focused unit test for post-submission completion observation, specifically proving a prior terminal status cannot satisfy a newly submitted operation.
- One cohesive Flowdex integration path that completes an initial child turn, exercises `keep`, `compact`, and `handoff`, and proves the handoff result belongs to a fresh thread with the new instructions.
- Existing focused StatusOnly completion-delivery tests if shared completion code changes.
- Scoped formatting, fixes, checks, and `git diff --check` for touched crates.

The integration path should also prove that the compacted turn starts only after compaction and that no intermediate handoff/completion message reaches the parent model request. Do not add a separate test for every validation branch, duplicate existing compaction suites, or run the full workspace suite unless a focused failure indicates it is necessary.

On Windows, use the established larger Rust test-thread stack for focused core integration tests when needed.

## Final review

After the complete path works, use one `gpt-5.6-luna` reviewer at `xhigh` reasoning for a single cohesive review. Ask it to focus on:

- Stale terminal-status races around resumed turns and compaction.
- Whether compaction actually completes before the next instruction is submitted.
- Fresh-thread history and resolved configuration inheritance in handoff mode.
- StatusOnly isolation in legacy and MultiAgentV2 completion paths.
- Cancellation, capacity, depth, and bounded-output preservation.
- Hidden-tool visibility and documentation accuracy.

Address actionable findings in scoped commits, rerun only affected verification, then stop reviewing.

## Explicit non-goals

This batch does not add:

- Runs, phases, tasks, dependencies, dynamic queues, or scheduling.
- Worktrees, commits, integration, attribution, or SQLite state.
- Verification/review loops or role-specific behavior.
- Tool profiles or new agent profile configuration.
- Handoff files, durable replacement aliases, or resume-after-process-restart behavior.
- Custom handoff prompts, retry policies, timeouts, context thresholds, or compaction strategies.
- Generic event buses, signals, human boundaries, or UI changes.
- Compatibility shims, feature flags, placeholders, or alternate runtimes.

## Completion report

When the batch is complete, report:

- Commits and changed files.
- The final JavaScript API and exact result behavior.
- How each context mode transforms the target thread.
- How newly submitted operations are distinguished from an old terminal status.
- Focused verification results and reviewer disposition.
- Any constraints the next plan must preserve.

Keep the worktree clean, compact context at the batch boundary, and send the report to the planning thread. Do not design Plan 009 in the implementation task.
