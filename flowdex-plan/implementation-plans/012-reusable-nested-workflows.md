# Flowdex Implementation Plan 012: Reusable and Nested Workflows

## Outcome

The saved JavaScript format becomes useful for both one-shot implementation plans and reusable workflow components. A workflow can resolve a named repository or global workflow, validate that workflow's declared input, invoke it conditionally, await it without a model turn, and receive a JSON-compatible result.

This batch builds on the Batch 010 scheduler. It must not replace `startRun`, duplicate phase/task execution, or add a second JavaScript runtime. A child workflow that calls `startRun` uses the same scheduler, worktree, verification, progress, agent-event, and integration machinery as a top-level workflow.

## Settled starting point

- Saved workflows already execute as native V8 code-mode cells. The cell `CellId` is the workflow/run controller identifier.
- `flowdex.startRun(...)` is the final high-level run/phase/task entry point from Batch 010.
- `start_flowdex_workflow({ path, input? })` and `wait_flowdex_workflow({ run_id })` remain direct-model tools.
- Scheduler progress is automatic, live-only, and never model- or workflow-callable.
- The accepted low-level task and agent APIs remain deliberate advanced primitives.

Do not introduce an alternate workflow definition object, package system, import system, runtime registry, compatibility alias, feature flag, or placeholder API.

## Final JavaScript additions

The public saved-workflow additions in this batch are:

```js
const input = flowdex.requireInput({
  properties: {
    files: { type: "array", items: { type: "string" } },
    strict: { type: "boolean" },
  },
  required: ["files"],
});

const documentation = await flowdex.runWorkflow(
  "global:documentation/check",
  { files: input.files, strict: input.strict ?? false },
);

flowdex.output({ documentation });
```

These names and shapes are the intended final API, not a preview for later replacement.

### `flowdex.requireInput(schema)`

- Synchronously validates the current `flowdex.input` and returns it.
- An omitted start input is treated as an empty object for validation.
- The root schema is an object declaration with exactly `properties` and optional `required`.
- Unknown schema fields are rejected.
- Unknown input fields are rejected by default; do not add a redundant `additionalProperties` option.
- Value schemas support only the JSON shapes needed for workflow input: `string`, `number`, `integer`, `boolean`, `array` with `items`, and nested `object` with `properties` and optional `required`.
- Reject malformed schemas, duplicate or unknown required names, type mismatches, non-JSON values, and non-plain input objects with path-specific bounded errors.
- Do not add coercion, defaults, unions, references, formats, custom validators, or a general JSON Schema engine.
- The call belongs at the beginning of a workflow before it performs work. Workflows that do not need a declared input may continue reading `flowdex.input` directly.

Keep this validator in the shared `codex-flowdex` bootstrap so local and remote native V8 execution use the same behavior without a hidden tool round trip.

### `flowdex.output(value)`

- Accepts one JSON-compatible value and writes its exact JSON serialization through the existing code-mode output channel.
- Returns JavaScript `undefined` and rejects a second call in the same workflow.
- Rejects `undefined`, functions, symbols, bigints, cycles, and other non-JSON values before emitting output.
- It is a pure bootstrap helper, not a model tool or hidden Rust tool.
- A workflow invoked as a child returns this value to its parent. No output resolves to `null`.
- A reusable workflow that returns a value must not mix raw `text(...)` output with `flowdex.output(...)`; ambiguous or non-JSON child output is a bounded workflow error.

Do not add a result database, result event stream, or separate output protocol. The existing bounded code-mode result is the transport.

### `flowdex.runWorkflow(workflow, input?)`

- `workflow` is a non-empty explicit reference using `repo:` or `global:`.
- `input` defaults to `{}` and must be a plain JSON-compatible object.
- Resolves and loads the child with the same hardened single-open file handling as top-level saved workflows.
- Starts a distinct native V8 cell, awaits it event-driven until terminal, parses its `flowdex.output(...)` value, and returns that value to the parent promise.
- Does not wake or call the orchestrator model between parent and child.
- Child JavaScript errors, invalid input, invalid output, cancellation, or scheduler failure reject the parent call with bounded error text.
- The child has the same hidden Flowdex primitives as a top-level saved workflow, including `startRun`; it cannot recursively call the direct `start_flowdex_workflow` tool.
- Parent cancellation terminates the child cell and reaches any child scheduler through the existing invocation cancellation token. Do not leave the child scheduler or task agents detached.
- User steering continues to wake the outer `wait_flowdex_workflow` path without cancelling the parent or child.

The runtime must reject a reference already present in the active parent chain. Track only the live parent-cell/reference chain needed for this check and clean it at terminal completion. Do not turn this into a generic event bus or durable call-stack framework.

## Named workflow resolution

Use one strict `WorkflowRef` representation in `codex-flowdex`:

- `repo:name/or/path` resolves beneath `<trusted repository>/.flowdex/workflows/name/or/path.js`.
- `global:name/or/path` resolves beneath `$CODEX_HOME/flowdex/workflows/name/or/path.js`.
- References contain normalized non-empty path segments only. Reject absolute paths, `.`/`..`, empty segments, alternate separators, drive or UNC syntax, embedded extensions, and NUL characters.
- Scope is always explicit. Repository files never shadow global files and resolution never falls back between roots.
- Repository resolution and saving require the repository to be trusted. Trust remains owned by Codex core; `codex-flowdex` receives only eligible roots.
- Preserve the existing canonical containment, regular-file, final symlink/reparse rejection, bounded source size, and bounded error behavior from the Batch 001 loader.

`start_flowdex_workflow.path` remains the existing field, but it may now contain an explicit `repo:` or `global:` reference. Existing repository-relative entry paths remain supported because they are the accepted one-shot workflow entry seam; nested workflows use only explicit references.

## Scoped workflow saving

Add one direct-model-only tool:

```text
save_flowdex_workflow({ workflow, source })
  -> { workflow }
```

- The schema has exactly the two required string fields above and rejects additional properties.
- `workflow` uses the same `WorkflowRef` parser as execution.
- `source` must be non-empty and obey the loader's source-size bound.
- Repository writes are trust-gated and rooted beneath `.flowdex/workflows`; global writes are rooted beneath `$CODEX_HOME/flowdex/workflows`.
- Create only the required named parent directories and write/replace the exact `.js` target safely. Reject symlink, junction/reparse, non-regular, or containment-uncertain targets.
- Return the normalized reference so the model can immediately pass it to `start_flowdex_workflow.path`.
- Do not expose arbitrary filesystem paths or add list/delete/rename operations in this batch.
- The save tool is not available inside saved workflows or ordinary code mode.

This is a normal source-writing operation. It does not execute the workflow, create a scheduler run, or make a model inference.

## Parent/child identity and scheduler reuse

- The child `CellId` is its authoritative run ID, just like a top-level workflow.
- Carry the parent cell ID and normalized workflow reference as internal bootstrap metadata.
- When a child calls `startRun`, extend the existing run row with its parent run ID and normalized workflow identity. Do not create another database or duplicate scheduler tables.
- A generic child that never calls `startRun` remains a live code-cell invocation and does not need a fake scheduler graph or empty phase rows.
- Automatic progress remains owned by actual scheduler transitions. Nested execution itself may emit one small automatic start/completion/failure summary through the same live-only reasoning path, without entering model history.
- Workflow-spawned agents continue to emit the normal app-server lifecycle and graph events. StatusOnly suppresses only parent-model completion text.

## Code boundaries

- `codex-flowdex` owns `WorkflowRef`, root-contained resolution, shared load/save path rules, the small input validator bootstrap, output serialization helper, and minimal parent-run store fields. It remains independent of `codex-core`.
- `codex-core` owns trusted-root selection, the direct save tool, hidden nested-run handler, child code-cell execution/observation, cancellation, active-chain tracking, automatic nested summaries, and tool visibility.
- Reuse `execute_source`, the no-deadline code-mode observer, existing dispatch/trace cleanup, and existing bounded Flowdex result mapping. Extract a narrow helper if necessary; do not copy the wait implementation into a second lifecycle.
- The hidden nested handler is present only in saved Flowdex tool specs. It is absent from the parent model, ordinary `functions.exec`, general code mode, and the child's direct-tool surface.

## Implementation tasks and parallelism

The orchestration agent is gpt-5.6-sol on low. Use managed task worktrees and require each worker to commit a brief scoped summary. Workers are not alone in the codebase and must preserve concurrent edits.

After confirming the exact shared Rust types, dispatch these two implementation tracks in parallel:

1. **Resolver and JavaScript contract — `implementation-worker`:** own `codex-rs/flowdex` loader/bootstrap changes and focused crate tests for `WorkflowRef`, strict input validation, and output serialization.
2. **Nested execution and saving — `implementation-worker`:** own the core Flowdex handlers/spec registration, event-driven child lifecycle, cancellation/cycle handling, and direct save tool. Code against the settled resolver API rather than inventing a second parser.

The orchestration agent should integrate the tracks, add the minimal parent-run store wiring, and resolve shared registration changes. Once the cohesive path works, a fast worker may update the Flowdex documentation and examples in parallel with the focused integration test.

Do not serialize independent worker tasks merely for convenience, but do not split individual files between workers.

## Focused verification and review

Tests exist here to prove the new cross-runtime behavior, not to decorate the plan:

- one focused `codex-flowdex` test group proves explicit repo/global resolution, traversal/link rejection, the intentionally small schema vocabulary, and exact JSON output behavior;
- one cohesive core integration starts a parent workflow, conditionally invokes a named child with declared input, has the child run a small scheduler-backed task or verification operation, returns JSON to the parent, and completes with no intermediate model request;
- that same path should confirm parent/child IDs, automatic live summaries, normal app-visible child-agent events, and StatusOnly model isolation;
- add narrow cancellation and active-cycle cases at the handler level because those failures can otherwise leak live child work;
- run affected Flowdex/code-mode tests, crate checks, formatting, and `git diff --check`; do not run the full workspace suite unless a concrete failure warrants it.

After the complete parent-to-child path passes, use exactly one gpt-5.6-luna reviewer on xhigh. The review has a specific purpose: inspect root/path safety, active-chain cleanup, cancellation propagation, child dispatch/trace cleanup, result isolation, hidden-tool visibility, and accidental duplication of scheduler behavior. Fix actionable findings and rerun only affected checks.

## Explicitly deferred

- task/phase/run human boundaries and named workflow signals;
- verification repair and structured review routing;
- context packs and automatic explorer dispatch;
- tool profiles;
- review-history candidate promotion (the parallel AST-grep runtime is independent);
- process-restart controller resumption and orphan administration;
- workflow list/delete/rename/catalog APIs;
- installer work;
- compatibility shims, feature flags, placeholders, and alternate runtimes.

## Documentation and handoff

Update the Flowdex documentation with:

- one complete reusable workflow using `requireInput` and `output`;
- one parent workflow conditionally calling it with `runWorkflow`;
- repository/global locations and explicit reference rules;
- the save/start flow used by the model;
- the fact that nesting is event-driven and model-silent while agents and automatic progress remain visible in the app.

Copy this plan unchanged into the implementation worktree before starting and verify its hash. When the batch is complete, send the planner a concise delegation containing commits, exact final API/result behavior, focused verification, reviewer findings, remaining constraints, and a clean-worktree statement. Do not design the next plan in the implementation task. Call `compact_context` at the batch boundary.
