# Flowdex Living Roadmap

Last updated: 2026-07-19

This is a planning scratchpad, not an implementation specification. Its job is to keep the remaining work pointed at the same final system while allowing individual implementation plans to change order when the code makes another sequence more sensible.

Concrete implementation plans remain the authority for a batch. `PLAN.md` remains the authority for product scope.

## North star

A model writes a saved JavaScript workflow and starts it. The workflow declares reusable agents, sequential phases, dependency-aware tasks, verification, review, context needs, and boundaries. It may also invoke named repository or global workflows with validated inputs. Flowdex then runs the resulting workflow tree programmatically while the orchestrator model sleeps.

The runtime, rather than the orchestrator, owns ready-task scheduling, subagent launches, task worktrees, verification, review routing, phase transitions, automatic progress, and event-driven waiting. It wakes the orchestrator only for a configured escalation, human boundary, user steering, or terminal workflow result.

JavaScript is the workflow definition and control language. The final authoring experience should be expressed in workflow/run/phase/task terms, not require authors to manually reconstruct a scheduler from isolated agent and verification calls.

## Status meanings

- **Complete:** implemented, documented, and accepted by the orchestration thread.
- **In progress:** an implementation batch is active and has not yet been accepted.
- **Partial:** useful final machinery exists, but the requested feature is not complete at its intended level.
- **Remaining:** no accepted implementation slice provides the feature yet.
- **Correction needed:** existing behavior conflicts with a later clarified requirement and should be changed directly, without a compatibility layer.

## Current implementation map

### Complete foundations

- **Saved workflow execution (Batch 001):** repository-local JavaScript workflow loading, native V8 execution, `start_flowdex_workflow`, bounded results, and the existing code-mode cell lifecycle.
- **Generic workflow agent bridge (Batch 002):** saved workflows can spawn agents, send messages, and wait without polling. Flowdex children reuse Codex's normal agent implementation and suppress only automatic parent-model completion delivery.
- **Silent verification primitive (Batch 003):** sequential command verification reuses Codex shell policy, hooks, approvals, sandboxing, cancellation, and bounded failure output. Passing commands do not cause another model inference.
- **Event-driven workflow-cell wait (Batch 005):** yielded workflows can be observed without a timer, and pending waits wake for user steering or trigger-turn mailbox input without consuming that input.
- **Native context compaction (Batch 006):** direct-model `compact_context({})` schedules native compaction at the post-tool boundary on the calling thread.
- **Compaction reminder (Batch 007):** global and trusted-repository configuration supplies a 150,000-token default threshold, and the model is reminded once per compaction window at the normal inference boundary.
- **Agent context reuse (Batch 008):** `keep`, `compact`, and fresh-sibling `handoff` reuse modes own exact submitted operations and preserve status-only isolation.
- **Task worktrees and commit attribution (Batch 009):** task agents run in isolated detached worktrees, reserve exact operation ownership before submission, record source commits with agent/model/summary attribution, bind verification to the task HEAD, and integrate atomically with source-to-integrated commit mappings. Scopes remain advisory, while worktree cleanup and uncertain conflict recovery preserve durable evidence.
- **Executable workflow scheduler (Batch 010):** `flowdex.startRun(...)` now executes durable runs with sequential phases, dependency-ready parallel tasks, advisory scope conflict serialization, dynamic open-phase queues, phase/run verification, deterministic integration, automatic non-model progress, app-visible StatusOnly agents, and strict final run/phase/task definitions.
- **Reusable and nested workflows (Batch 012):** repository/global workflow references, strict declared JSON input, exact JSON output, event-driven nested V8 execution, durable parent/child identity, cancellation, and rooted atomic saving are complete.
- **AST-grep rule runtime (Batch 011, integrated after Batch 012):** approved repository rules, strict global/repository configuration, explicit workflow checks, automatic post-command verification, bounded findings, and trusted-root/task-worktree separation are complete on the authoritative scheduler line.
- **Desktop app-backend installer (Batch 014):** `codex flowdex install --binary <path>` validates a compiled Codex executable and persistently selects it through `CODEX_CLI_PATH` for the current Windows or macOS user. Windows uses the user environment registry; macOS maintains an idempotent marked block in the supported login-shell profile. Other platforms and unsupported shells fail without mutation.
- **Context packs (Batch 013):** workflows declare named packs and task requirements; immutable fragments are persisted with source hashes and supersession, missing or stale packs dispatch one ordinary collector, unrelated ready tasks continue concurrently, and only dependent task prompts receive bounded fresh context.

### Partial or correction needed

- **Workflow authoring API:** run/phase/task scheduling, reusable nested workflows, and context requirements are settled. Review/repair composition, boundaries, and tool profiles remain additive final-language slices.
- **Workflow event wait:** waiting for the V8 cell and user steering works. Waiting on durable workflow, phase, task, checkpoint, command, and explicit-signal events belongs to the scheduler that will own those states.
- **Configuration:** compaction and AST-grep settings use strict global/repository precedence. Tool profiles, agent defaults, and round limits remain.

### In progress

- **Review, repair, and boundaries (Batch 015):** add role-neutral task verification repair, structured task/phase review attribution, direct finding routing, independent budgets, and event-driven orchestrator/human boundaries. Generic multi-agent rounds remain ordinary JavaScript loops over the existing messaging/resume primitives rather than a reviewer-specific runtime abstraction.

## Remaining durable feature slices

These are capability groups, not a mandatory batch order. Before implementing one, settle the part of its final public contract that its code owns. Do not create a temporary public API with a planned replacement.

### 1. Final workflow definition contract

Define the durable JavaScript authoring vocabulary for runs, reusable agents, phases, tasks, dependencies, verification, review, context requirements, boundaries, and strict workflow input schemas. It should support both initially declared work and later dynamic additions without exposing internal paths, database rows, operation IDs, or scheduler bookkeeping.

This is the contract the later scheduler implements. Existing low-level primitives should either compose cleanly beneath it, remain deliberate advanced escape hatches, or be changed in place.

The run/phase/task portion is complete in Batch 010. The remaining vocabulary is delivered with the feature slices that consume it rather than accepted as inert schema fields.

### 2. Persistent run/phase/task scheduler

Turn a validated definition into an executing run:

- Run phases sequentially.
- Run every dependency-ready task concurrently by default, bounded by the existing agent-capacity limits. Dependencies express required ordering; authors should not need a separate parallel-task wrapper.
- Use declared write scopes to avoid obvious concurrent write/write conflicts, but keep scopes advisory rather than treating them as edit permissions. Task worktrees isolate unexpected overlap, and commit integration remains the final conflict boundary.
- Inherit phase instructions into owned tasks.
- Allow tasks to be queued while an active phase is open.
- Validate a dynamic addition without stopping unrelated work when that addition is invalid.
- Seal a phase before phase verification, review, and boundary handling.
- Preserve enough state in the per-repository SQLite database to inspect and continue the run without inventing a generic event framework.

This should consume the task/worktree lifecycle from Batch 009 rather than replace it.

Complete in Batch 010 for live-process execution. Process-restart controller resumption remains deferred.

### 3. Reusable and nested workflow composition

Use one saved JavaScript workflow format for top-level implementation plans and reusable generic workflows:

- Store repository workflows beneath `.flowdex/workflows/` and global workflows beneath `$CODEX_HOME/flowdex/workflows/`.
- Address workflows with explicit `repo:` or `global:` scope rather than implicit shadowing.
- Let each workflow declare a strict JSON-compatible input object schema and reject invalid input before execution.
- Let JavaScript conditionally invoke a child workflow, pass input, await it event-driven, and receive a JSON-compatible result without waking the orchestrator model.
- Record parent/child run identity and reuse the scheduler, automatic progress, normal app-server agent visibility, cancellation, and steering behavior.
- Reject active-chain invocation cycles. Do not add Node, a second runtime, a package manager, or a generic module system.
- Add one narrowly rooted model operation for saving or updating a named repository or global workflow; keep repository access trust-gated.

This composes the durable scheduler rather than replacing `startRun` or duplicating its phase/task machinery.

### 4. Automatic runtime visibility

Attach user-visible updates to real scheduler transitions. Use small deterministic summary templates for phase start/completion, task start/completion, verification, review rounds, repair, checkpoints, escalation, and workflow completion.

Requirements:

- No model call and no workflow-authored progress call.
- No conversation-history, rollout-history, resume-state, workflow-result, or next-request inclusion.
- Continue using the existing app-server reasoning-summary channel.
- Preserve ordinary Codex subagent lifecycle events so workflow-spawned agents remain visible in the Codex app.
- Status-only completion suppresses parent-model injection only; it must not suppress GUI events, status, graph metadata, or captured output.
- Emit meaningful transitions and coalesce repetitive updates.

The explicit progress API should be removed or made wholly internal as part of this correction; do not preserve it through a compatibility shim.

### 5. Boundaries, suspension, and workflow events

Implement task, phase, and run boundaries for automatic continuation, orchestrator escalation, and human approval/revision. Extend event-driven suspension to scheduler-owned completion, checkpoints, explicit workflow signals, and escalations.

User steering must always interrupt an active wait. Steering does not implicitly cancel the run; it returns control to the orchestrator, which can decide what to do.

### 6. Verification repair and review composition

Build the final task/phase orchestration around the existing verification primitive:

- Verification failure routes bounded command results to the workflow-selected repair agent.
- Verification and review have separate round budgets.
- Review is an ordinary agent operation, not a runtime role.
- A structured report records file, line range, reason, optional stable rule key, and AST-grep suitability.
- Findings route through line-level commit attribution, then file-level attribution, then orchestrator escalation.
- Review agents communicate directly with the attributed implementation agent when routing succeeds.

The runtime supplies composition and routing primitives without hard-coding a worker/reviewer loop.

### 7. Context packs — complete (Batch 013)

Add versioned, immutable context fragments grouped into named packs. A newer fragment supersedes an older one; integrated commits that touch covered lines mark affected fragments stale. Later task dispatches resolve the newest non-stale fragment and inject it directly into the agent prompt without passing its content through the orchestrator.

If required context is absent or stale, queue a bounded context-gathering task, suspend only dependent tasks, and resume them when the fragment is published. Escalate only when collection fails. Keep fragments and injected packs bounded.

### 8. Agent and tool profiles

Complete workflow-level reusable agent declarations using model, reasoning effort, existing `.codex/agents` profiles, and optional Flowdex tool profiles. Agent roles remain instructions and tool access, never worker/reviewer/explorer runtime enums.

Expand global and trusted-repository configuration only for settings actually consumed by completed features: named tool profiles, agent defaults, context gathering, round limits, and AST-grep behavior.

### 9. Review history and AST-grep rule promotion

Persist findings and the commits that resolve them. Group repeated resolved findings by the reviewer-provided stable rule key and expose candidates after the configured repetition count. Do not add model-based background clustering.

A user-started action may dispatch the future rule-writing agent or skill. Each repository rule requires individual human approval and is then configured as always-on verification or explicitly requested by a workflow.

### 10. Cohesive completion pass

Once the real workflow pipeline exists, exercise one representative workflow covering phase inheritance, parallel dependency-ready tasks, dynamic queuing, direct agent messages, worktrees, verification/repair, review routing, context collection, integration, boundaries, automatic progress, steering, and completion.

Use that pass to close genuine integration gaps and make the Flowdex documentation describe the final authoring API and runtime behavior. Avoid expanding it into exhaustive tests for every schema field or duplicating checks already owned by Codex primitives.

## Likely dependency shape

The broad direction is:

1. Settle the final workflow definition contract.
2. Build the scheduler on the accepted task/worktree foundation.
3. Connect automatic progress, app-server visibility, and scheduler waits to its real state transitions.
4. Add boundaries and verification/review composition.
5. Add context packs once dynamic task suspension/resumption exists.
6. Add profile/configuration completion and AST-grep promotion where they naturally fit.
7. Finish with the cohesive workflow pass and final usage documentation.

This order is intentionally movable. For example, the automatic event emitter and app-server propagation can be implemented in parallel with scheduler storage once the scheduler transition vocabulary is settled. Context persistence and prompt injection can also be divided after the fragment schema is final.

## Parallel implementation opportunities

Future concrete plans should prefer parallel workers when file ownership and contracts are independent. Reasonable splits include:

- workflow JavaScript schema/bindings and Rust scheduler/store implementation after their shared data contract is fixed;
- scheduler transition emission and app-server event propagation after the transition vocabulary is fixed;
- structured review persistence and attribution routing;
- context-fragment storage and agent-prompt injection;
- CLI installer behavior and its focused documentation.

Central session-loop changes, shared tool registration, and migrations that depend on one unsettled schema should remain serial. Parallelism should shorten independent work, not manufacture merge conflicts.

## Planning guardrails

- Implement final feature slices, not demonstrations or scaffolding planned for replacement.
- Keep schemas minimal; every field must solve a stated requirement.
- Reuse Codex execution, agent, event, sandbox, approval, compaction, and app-server machinery.
- Keep file scopes advisory.
- Make dependency-ready task parallelism the scheduler default; add an explicit parallel primitive only if a concrete workflow cannot express its ordering through dependencies.
- Keep agents role-neutral.
- Keep JavaScript native to the existing V8 runtime; do not add Node.
- Do not add compatibility shims, feature flags, placeholders, a visual editor, GUI-specific modifications, or a generalized orchestration graph/event bus.
- Keep one final cohesive reviewer per working batch unless a concrete high-risk issue warrants more.
- Update this roadmap when a batch is accepted: move its capability to **Complete**, record any settled constraints, and adjust remaining slices when implementation evidence changes the route.

## Update log

- **2026-07-19:** Accepted and integrated Batch 013 after Batch 014. Context packs now provide persisted immutable fragments, fresh/missing/stale resolution, automatic single collector dispatch, safe same-handle source reads, supersession, bounded task-only injection, and concurrent context preparation that does not block unrelated ready work.
- **2026-07-19:** Accepted and integrated Batch 014 onto `codex/flowdex`. The installer now configures the current-user Codex desktop backend on Windows and macOS with strict executable identity checks and no scheduler/store coupling. The macOS path is statically and unit tested but still needs a real-host acceptance run when one is available.
- **2026-07-19:** Anchored the accepted Batch 010/012 line plus integrated Batch 011 as the canonical `codex/flowdex` branch. Batch 012 reusable/nested workflows and Batch 011 AST-grep verification are complete. Prepared Batch 013 context packs and the disjoint Batch 014 Windows installer for parallel execution from that exact baseline.
- **2026-07-18:** Accepted Batch 010: the final `startRun` scheduler now executes sequential phases, dependency-ready parallel tasks, dynamic queues, verification, integration, automatic progress, and app-visible StatusOnly agents. Started Batch 012 for reusable repository/global workflows and event-driven nesting on that settled API.
- **2026-07-18:** Started parallel Batch 011 for the independent AST-grep accepted-rule runtime, with focused behavioral verification and one cohesive risk-based review rather than a broad test/reviewer matrix.
- **2026-07-18:** Added reusable repository/global workflows, declared input schemas, conditional nested invocation, parent/child run semantics, and scoped workflow authoring to the long-horizon design.
- **2026-07-18:** Initial roadmap created from accepted Batches 001-008, the active Batch 009 implementation, the master Flowdex plan, and the clarified requirements for automatic progress and app-visible subagents.
- **2026-07-18:** Clarified that independent dependency-ready tasks dispatch in parallel by default, within agent capacity, with declared write scopes used only as conflict-avoidance hints.
- **2026-07-18:** Accepted Batch 009 and recorded task worktrees, exact operation/commit attribution, exact-HEAD verification, atomic integration, and conservative recovery as completed scheduler foundations.
- **2026-07-18:** Started Batch 010 for the executable workflow-language and scheduler slice, beginning with removal of the callable progress API and then using parallel workers for the store and JavaScript contract.
