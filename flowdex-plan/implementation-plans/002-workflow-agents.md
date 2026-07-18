# Flowdex Implementation Plan 002: Workflow Agents

## Outcome

A saved Flowdex workflow can create a general-purpose Codex sub-agent, send it a message, and await its terminal status entirely inside the running JavaScript workflow.

This batch is the smallest useful agent-orchestration slice. It does not introduce tasks, phases, reviewers, verification, persistence, or a scheduler. Those can be built later by composing these primitives after their actual behavior is known.

## Starting Point

Batch 001 established the boundaries this plan extends:

- `start_flowdex_workflow` loads repository JavaScript and executes it through the existing V8 `CodeModeService`.
- The V8 `CellId` is the Flowdex run/controller identifier.
- The loader creates the reserved, frozen `flowdex` bootstrap object.
- The start tool owns a captured list of nested tool specifications and is excluded from that list, so a workflow cannot recursively start another workflow.
- Nested tool calls already travel through the normal `ToolRouter` with the current session and turn context.

Keep those boundaries. Do not add a second runtime, run registry, polling loop, or scheduler.

## Data Transformations

This batch should be implemented as four direct transformations:

1. A JavaScript agent specification passed to `flowdex.spawnAgent(...)` becomes the existing Codex agent configuration plus initial input.
2. The existing `AgentControl` creates the child and returns its `ThreadId`; the wrapper exposes that identifier to the workflow as a string.
3. `flowdex.sendMessage(...)` converts a target id, message, and delivery mode into the existing `InterAgentCommunication` operation.
4. `flowdex.waitAgent(...)` converts a target id into an `AgentControl::subscribe_status` watch and resolves only when that watch reaches an existing terminal `AgentStatus`.

Agent output must travel back through `waitAgent`, not through an automatic completion message injected into the parent model context. The JavaScript workflow can then route the result directly to another agent or return its final result.

## JavaScript Contract Introduced

Add these functions to the existing frozen `flowdex` object:

```js
const agentId = await flowdex.spawnAgent({
  name: "implement-parser",
  instructions: "Implement the parser changes described in the workflow input.",
  profile: "implementation_worker",
  model: "gpt-5.6-luna",
  reasoningEffort: "high",
});

await flowdex.sendMessage(agentId, "Check the new byte-layout requirement.", {
  delivery: "turn",
});

const result = await flowdex.waitAgent(agentId);
```

### `flowdex.spawnAgent(spec)`

Required fields:

- `name`: stable task/agent name used by the existing agent graph.
- `instructions`: initial worker instructions.

Optional selector fields:

- `profile`: an existing custom agent profile resolved through the normal `.codex/agents` mechanism.
- `model`: model override.
- `reasoningEffort`: reasoning-effort override.

At least one selector field must be present. Do not add a Flowdex role enum: profiles remain ordinary Codex agent profiles, and agents are not classified as workers or reviewers.

Do not expose fork-context, service-tier, tool-profile, reuse, or worktree options in this batch. The child starts from the normal non-forked agent configuration and receives only its explicit instructions plus existing agent-profile instructions.

Return the child `ThreadId` as a string. Avoid a wrapper handle until the workflow needs more data than the id.

### `flowdex.sendMessage(agentId, message, options?)`

`options.delivery` has two values:

- `"queue"`: append the message without starting another agent turn.
- `"turn"`: start or continue a recipient turn with the message.

Default to `"queue"`. This preserves the low-cost path when the recipient is already working, while allowing a workflow to wake a completed implementation agent for a repair round. Use the existing `InterAgentCommunication` mechanism for both; the only behavioral difference is its existing `trigger_turn` value.

Return only the existing submission acknowledgement needed to diagnose delivery. Do not copy the message into the parent model context.

### `flowdex.waitAgent(agentId)`

Subscribe to the target agent status and wait without a timeout or polling interval. Resolve immediately if the status is already terminal. Return one bounded object:

```js
{
  agentId: "...",
  status: "completed" | "errored" | "shutdown" | "notFound",
  message: "..." // present for completed or errored status when available
}
```

Use the existing final-status definition. An interrupted agent is not terminal and therefore does not resolve `waitAgent` until it later reaches a terminal state.

JavaScript already provides `Promise.all` and `Promise.race`, so do not add separate wait-any or wait-all APIs.

## Rust Integration Boundaries

### Private nested tools

Back the three JavaScript functions with narrowly scoped nested tools. Register their runtimes with `ToolExposure::Hidden`, then explicitly append only their specifications to the nested tool list captured by `StartFlowdexWorkflowHandler`.

This means:

- They are callable from a started Flowdex workflow.
- They are not model-visible direct tools.
- They are not included in ordinary `functions.exec` code mode.
- `start_flowdex_workflow` remains excluded from its own nested tools.

Keep the JavaScript wrappers in `codex-flowdex`, beside the existing bootstrap construction. Keep session-aware execution in `codex-core`; `codex-flowdex` must remain independent of `codex-core`.

### Agent creation

Reuse the existing spawn configuration functions used by the current multi-agent handler for:

- base child configuration,
- custom profile resolution,
- model and reasoning-effort overrides,
- spawn-depth and execution-capacity checks,
- agent path/name registration,
- initial input submission.

If a small helper must be made `pub(crate)` or extracted so the Flowdex handler and existing handler share it, do that instead of duplicating the configuration sequence.

Use the workflow's current session/thread as the parent and the current session source to derive the sender agent path. A workflow started by a sub-agent must therefore create and message descendants of that sub-agent rather than pretending to be root.

### Completion delivery

Today, a normal thread-spawn child starts a watcher that queues its terminal result to its parent. Flowdex needs the same child registration but a different terminal delivery route.

Extend `SpawnAgentOptions` with a small explicit completion-delivery enum, not a boolean:

- normal/default behavior: notify the parent as today;
- Flowdex behavior: status-only, because `flowdex.waitAgent` owns delivery.

The default must preserve all existing multi-agent behavior. Status-only must skip only the automatic parent notification; it must not disable status tracking, persisted parent/child edges, agent metadata, or final-message capture.

### Cancellation

The wait future must remain attached to the nested tool invocation's existing cancellation path. Terminating or cancelling the V8 cell/turn must drop the status wait; do not create a detached task that outlives the workflow.

This batch does not implement the separate model-facing Flowdex suspension tool or its user-steering wake behavior. It establishes the event-driven agent-status wait that that later feature can compose with.

## Work Orders

### Task 1: Status-only child completion

Use one `implementation_worker_fast`.

Scope:

- `codex-rs/core/src/agent/control.rs`
- `codex-rs/core/src/agent/control/spawn.rs`
- the existing agent-control test file only if focused coverage is needed

Instructions:

1. Add the explicit completion-delivery choice to `SpawnAgentOptions`, defaulting to current parent notification behavior.
2. Pass the choice to the existing completion-watcher boundary.
3. In status-only mode, retain status observation and all child bookkeeping but do not send or inject a terminal message into the parent.
4. Update existing struct literals without changing their behavior.
5. Commit the task with a brief summary before returning it to the orchestrator.

Acceptance:

- Existing agent spawns still notify their parents exactly as before.
- A status-only spawn reaches a terminal status without changing the parent's model-visible history.
- No compatibility shim or feature flag is introduced.

Verification:

- Run the narrow agent-control test that covers the completion watcher.
- Add at most one focused test for status-only behavior if existing coverage cannot express it.

### Task 2: Flowdex agent bridge and JavaScript wrappers

Use one `implementation_worker` after Task 1 is committed.

Scope:

- `codex-rs/flowdex/src/`
- a new focused module under `codex-rs/core/src/tools/flowdex/` if needed
- `codex-rs/core/src/tools/flowdex.rs`
- `codex-rs/core/src/tools/spec_plan.rs`
- existing multi-agent spawn helpers only where a small shared extraction is necessary
- Flowdex-focused core integration tests

Instructions:

1. Add the three JavaScript wrappers with the contract above to the reserved `flowdex` bootstrap.
2. Implement hidden nested handlers for spawn, message, and event-driven terminal wait.
3. Add those three specs only to the Flowdex start handler's nested tool set.
4. Use existing argument parsing, `AgentControl`, profile resolution, model override, inter-agent communication, status watch, and bounded output conventions.
5. Spawn Flowdex children with status-only completion delivery.
6. Keep the current `CellId` execution lifecycle and start-tool recursion exclusion intact.
7. Prefer a new module rather than substantially growing the existing `flowdex.rs` tool handler.
8. Commit the task with a brief summary before returning it to the orchestrator.

Acceptance:

- A saved workflow can spawn a custom-profile or explicitly selected model agent.
- The workflow receives a thread id and can address that agent generically.
- Queue delivery does not start a turn; turn delivery does.
- Waiting is status-watch driven and returns the terminal message/error to JavaScript.
- Agent completion does not wake the parent model or append a completion item to its context.
- Ordinary `functions.exec` cannot invoke the hidden Flowdex agent tools.
- The direct start tool remains unavailable inside the workflow.

Verification:

- Add one end-to-end Flowdex core integration covering spawn, a workflow-owned wait, and successful final output without an intermediate parent model turn.
- Cover the hidden-tool boundary and completion-context behavior in that test when practical rather than adding separate broad test suites.
- Add one narrow message-delivery assertion only if the end-to-end flow cannot distinguish queue and turn delivery.

### Task 3: Current agent-workflow documentation

Use one `implementation_worker_fast` after Task 2.

Scope:

- `flowdex-plan/flowdex-documentation/`
- this Plan 002 file copied unchanged into the implementation worktree

Instructions:

1. Document the actual `spawnAgent`, `sendMessage`, and `waitAgent` signatures and returned values implemented by Task 2.
2. Include one short saved-workflow example.
3. State the current limitations: no task/phase layer, no automatic review loop, no worktree assignment, no tool profiles, and no context-reuse modes yet.
4. Record any implementation detail the next planner needs if it differs from this plan.
5. Commit the documentation with a brief summary.

Acceptance:

- The documentation is sufficient to write a simple agent-orchestration workflow without reading Rust source.
- It describes only behavior that exists at the end of this batch.

## Orchestrator Integration and Review

The orchestration task should apply the three task commits in order and keep the worktree clean between tasks. Workers are not alone in the codebase and must preserve earlier commits and unrelated user changes.

After the full slice works:

1. Run `just fmt` from `codex-rs`.
2. Run focused tests for `codex-flowdex`, the Flowdex core integration, and any changed agent-control behavior.
3. Run `just fix -p codex-flowdex` and `just fix -p codex-core` as applicable.
4. Use one final `gpt-5.6-luna` reviewer at `xhigh` for the cohesive batch. Ask it to focus on parent-context leakage, hidden-tool exposure, message delivery semantics, cancellation, and reuse of existing spawn configuration.
5. Fix material findings in small commits; do not add new features in response to speculative suggestions.
6. Compact context at the batch boundary and message the planning task with commits, focused verification, actual API signatures, implementation notes, and remaining constraints.

Do not run the complete workspace test suite unless a focused failure shows that the changed shared behavior requires it or the user approves it.

## Non-Goals for Batch 002

- Task, phase, run, dependency, or dynamic-queue schemas.
- Worker/reviewer types or automatic review loops.
- Verification commands or round budgets.
- Worktree creation, commits, or attribution performed by Flowdex itself.
- SQLite runtime state.
- Context chunks or explorer dispatch.
- Context reuse, compaction, or structured handoffs.
- Model-facing workflow suspension and user-steering wake behavior.
- Reasoning-summary progress events.
- Tool profiles, Flowdex configuration, installer behavior, or AST-grep rules.
