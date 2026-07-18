# Flowdex Implementation Plan 004: Progress Summaries

## Outcome

A running Flowdex workflow can publish a concise progress summary that appears through Codex's existing reasoning-summary UI path without adding that text to conversation history, persisted rollout items, or a later model request.

This batch adds one explicit JavaScript primitive. It does not infer task or phase state and does not introduce a scheduler. Workflow code chooses meaningful transition points such as starting a unit of work, beginning verification, or reaching a checkpoint.

## Starting Point

Batch 003 established:

- Saved workflows run in the existing native V8 code-mode runtime.
- The frozen `flowdex` bootstrap exposes private hidden operations backed by nested tools.
- Agent waits and verification can keep a workflow active for a meaningful amount of time without an intermediate parent-model turn.
- Core handlers already have the current session and turn context needed to emit client events.

Codex already represents reasoning summaries as `TurnItem::Reasoning` lifecycle events. App-server and other clients understand that existing item shape. Ordinary `Session::send_event` persists events to the rollout, so Flowdex progress must use a deliberate client-delivery path that does not persist or record the item into model history.

## Data Transformation

The complete batch is one transformation:

```text
JavaScript progress text
    -> hidden Flowdex progress call
    -> bounded transient TurnItem::Reasoning
    -> existing client event channel
    -> no JavaScript value, rollout item, or model-visible history item
```

Do not create a Flowdex-specific GUI event or modify the Codex GUI. The existing reasoning item lifecycle is the compatibility surface.

## JavaScript Contract Introduced

Add one function to the existing frozen `flowdex` bootstrap:

```js
await flowdex.progress("Verifying the parser changes");
```

### `flowdex.progress(summary)`

`summary` is one string and must be non-empty after trimming. Emit the trimmed text. Bound it with an existing Codex text/output truncation helper rather than adding a new configuration field or an unbounded UI payload.

The promise resolves with `undefined`. The private tool may use the smallest internal acknowledgement required by code mode, but the bootstrap wrapper must consume it so workflows do not gain a redundant result object.

Do not add severity, phase, task, percentage, category, metadata, or formatting fields. A workflow can include useful wording in the summary itself.

## Transient Event Behavior

Represent each accepted call as one completed reasoning-summary item using the existing `TurnItem::Reasoning` shape and a unique item ID. Send the normal item lifecycle needed by current clients; do not add a new protocol variant.

The progress item is UI-only:

- deliver it to current event subscribers;
- do not call `record_conversation_items`;
- do not persist its started, delta, or completed events to the rollout;
- do not include it when reconstructing history or creating the next model request;
- do not return its text through the parent workflow tool result.

Add the smallest crate-private session helper needed to deliver this event without rollout persistence. Keep its semantics explicit in the name or encapsulate it as a Flowdex progress emitter. Do not weaken persistence for ordinary reasoning, tool, or message events.

Honor the existing client reasoning-display behavior and configuration. Do not add a Flowdex override for hidden reasoning.

This explicit primitive does not need automatic deduplication or rate limiting. The workflow author is responsible for calling it only at meaningful state transitions. Automatic scheduler-generated progress and coalescing belong with the future task/phase runtime, where the runtime will have state to coalesce correctly.

## Rust Integration Boundaries

### Private Flowdex tool

Back `flowdex.progress` with one hidden nested tool following the existing Flowdex visibility boundary:

- available only inside a started Flowdex workflow;
- not model-visible;
- unavailable to ordinary `functions.exec`;
- does not expose `start_flowdex_workflow` recursively.

Progress delivery requires no new shell, collaboration, or filesystem capability gate. It inherits the active workflow's session, turn, and cancellation lifecycle.

Keep only the JavaScript wrapper in `codex-flowdex`. Keep session event construction and delivery in `codex-core`; `codex-flowdex` must remain independent of core and protocol internals.

### Event delivery

Reuse the existing reasoning `TurnItem` and item started/completed event mapping so app-server needs no new notification mapping. If the existing helper always persists, add one narrowly scoped non-persisting delivery seam rather than manually sending on internal channels from the tool handler.

The transient path may still feed ordinary live tracing/telemetry if that is already inseparable from client delivery, but the progress item itself must not become rollout or conversation history. Do not create a second general-purpose event bus.

## Work Orders

### Task 1: Progress event bridge

Use one `implementation_worker`.

Scope:

- `codex-rs/core/src/tools/handlers/flowdex.rs`
- `codex-rs/core/src/tools/spec_plan.rs`
- the smallest session event-delivery module needed for transient delivery
- `codex-rs/flowdex/src/lib.rs`
- Flowdex-focused tests

Instructions:

1. Add the exact `flowdex.progress(summary)` wrapper and make it resolve to `undefined`.
2. Add one hidden Flowdex-only tool with a single `summary` string argument.
3. Validate, trim, and bound the summary with existing utilities.
4. Emit a synthetic reasoning item through the existing item lifecycle and current turn identifiers.
5. Deliver the item live without persisting it or recording it in conversation history.
6. Preserve all existing Flowdex tool visibility, recursion exclusion, and cancellation behavior.
7. Keep normal model reasoning and ordinary event persistence unchanged.
8. Do not add task, phase, status, percentage, or scheduler abstractions.
9. Commit the implementation with a brief summary before returning it to the orchestrator.

Acceptance:

- A saved workflow can publish progress while it is awaiting agents or running verification.
- Current Codex clients receive the progress as a reasoning-summary item without GUI changes.
- The summary is absent from conversation history, persisted rollout items, later model input, and the workflow's parent result.
- Existing non-Flowdex event persistence is unchanged.

Verification:

- Add one Flowdex integration test that invokes `flowdex.progress`, observes the expected reasoning item lifecycle, and still completes the workflow normally.
- In that same focused test, use the most direct available assertion to prove the summary is absent from model-visible history or the next model request.
- Add a narrow session-helper test only if the integration cannot directly prove non-persistence.
- Do not add separate tests for every invalid string shape or static schema field.

### Task 2: Progress documentation

Use one `implementation_worker_fast` after Task 1 is committed.

Scope:

- `flowdex-plan/flowdex-documentation/`
- `flowdex-plan/PLAN.md` only for the already-settled native V8/no-Node correction
- this Plan 004 file copied unchanged into the implementation worktree

Instructions:

1. Document the actual `flowdex.progress` signature and a short workflow example.
2. Explain that summaries are live, transient, UI-only reasoning items and are not available after resume.
3. State that explicit workflow calls currently choose transition timing; automatic task/phase progress does not exist yet.
4. Keep the current limitations accurate.
5. Preserve the corrected master-plan statement that workflows use native V8 and the installer has no Node prerequisite.
6. Commit the documentation with a brief summary.

Acceptance:

- A workflow author can publish a progress summary without reading Rust source.
- Documentation does not claim automatic task/phase progress or durable progress history.

## Orchestrator Integration and Review

Apply the task commits in order and keep the worktree clean between them. Workers must be told they are not alone in the codebase and must preserve Batch 001-003 work and unrelated user changes.

After the full progress path works:

1. Run `just fmt` from `codex-rs`.
2. Run focused `codex-flowdex`, Flowdex integration, and changed session-event tests.
3. Run `just fix -p codex-flowdex` and `just fix -p codex-core` as applicable.
4. Use one final `gpt-5.6-luna` reviewer at `xhigh`. Focus it on accidental rollout/history persistence, malformed reasoning item lifecycle, unbounded UI text, tool visibility, and changes to ordinary event delivery.
5. Fix material findings in small commits without expanding the feature.
6. Compact context at the batch boundary and message the planning task with commits, verification, the actual API behavior, event/persistence details, reviewer findings/fixes, and constraints relevant to Plan 005.

Do not run the complete workspace suite unless a focused failure makes it necessary or the user approves it.

## Non-Goals for Batch 004

- Task, phase, run, dependency, or dynamic-queue schemas.
- Automatic task/phase progress generation, deduplication, or coalescing.
- General workflow signals, multi-event waits, user-steering wake behavior, or human boundaries.
- Worktree creation, task commits, integration, or attribution.
- Reviews, structured findings, or automatic repair loops.
- SQLite runtime state.
- Context chunks, agent reuse modes, compaction, or handoffs.
- Tool profiles, configuration, installer implementation, or AST-grep rules.
