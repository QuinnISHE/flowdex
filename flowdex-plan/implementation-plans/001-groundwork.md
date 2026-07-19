# Flowdex Implementation Plan 001: Groundwork

## Outcome

Establish the smallest end-to-end Flowdex execution path:

1. The model writes a JavaScript workflow under `.flowdex/workflows/`.
2. The model calls `start_flowdex_workflow` with the repository-relative path and optional JSON input.
3. Codex reads the file, adds the minimal Flowdex bootstrap, and executes it in the existing native V8 code-mode session.
4. The tool returns the code-mode cell ID as the Flowdex run ID together with the current completed, yielded, failed, or terminated state.

This batch does not implement task scheduling yet. Its purpose is to establish the permanent workflow loading and execution seam that later batches will extend with Flowdex primitives.

## Why This Is the Groundwork

The repository already has the transformations Flowdex needs at its lowest level:

- `codex-code-mode` evaluates JavaScript as async modules in a V8 isolate.
- `codex-code-mode-host` supports process-owned execution.
- `CodeModeService` owns cells and already returns stable `CellId` values across yield/wait cycles.
- The code-mode delegate already exposes selected Codex tools to JavaScript without giving the isolate filesystem or network access.

Flowdex should therefore load saved workflow source into code mode, not add Node, another JavaScript engine, or a second cell lifecycle.

The data path for this batch is:

```text
{ path, input }
  -> validated repository workflow path
  -> UTF-8 JavaScript source
  -> Flowdex bootstrap + source
  -> code-mode ExecuteRequest using the current turn's nested-tool definitions
  -> RuntimeResponse
  -> { runId: CellId, status, output }
```

The cell ID remains the controller/run identifier in later batches. Durable run metadata can map to it when persistence is introduced.

## Public Contract Introduced

### Model tool

`start_flowdex_workflow`

Input:

- `path`: repository-relative path beneath `.flowdex/workflows/`; the file must use the `.js` extension.
- `input`: optional JSON value made available to the workflow.

Output:

- `runId`: the underlying code-mode cell ID.
- `status`: `completed`, `yielded`, `failed`, or `terminated`.
- `output`: the bounded content emitted by the workflow before the returned state.
- `error`: present only when execution completes with a JavaScript error.

The tool is a direct model tool and must not be exposed inside its own nested code-mode tool surface.

### Workflow bootstrap

The workflow source executes as an async module with a reserved top-level `flowdex` object:

```js
const flowdex = Object.freeze({
  input: /* JSON input or null */,
  workflowPath: /* repository-relative path */,
});
```

The loader prepends this object to the saved source using JSON serialization for both values. Workflow files may use top-level `await`, existing code-mode output helpers, and the nested `tools` object. Later batches extend `flowdex`; they do not replace this entry point.

### Path behavior

- Resolve paths with Codex path types, not URI types or lossy string concatenation.
- Canonicalize the workflow file and `.flowdex/workflows` root before reading.
- Reject absolute inputs, traversal, symlink escapes, non-JavaScript files, and missing files with a concise tool error.
- Do not copy workflow source into model-visible output.

## Work Orders

The orchestration agent owns sequencing and integration. Workers are not alone in the repository: they must preserve user changes, stage only owned paths, and never revert another worker's work.

Each worker must finish with a small commit whose subject briefly states what it added. Do not leave completed worker changes in a shared dirty worktree.

### Task 1: Fork instructions and documentation

Agent: `implementation_worker_fast`

Scope:

- Read: `ORIGINAL_AGENTS.md`, `flowdex-plan/PLAN.md`, this implementation plan.
- Write: `AGENTS.md`, `flowdex-plan/PLAN.md`, `flowdex-plan/flowdex-documentation/groundwork.md`.
- Do not edit Rust code or manifests.

Instructions:

- Write a concise root `AGENTS.md` for this fork. Use `ORIGINAL_AGENTS.md` only as a source of useful Codex conventions; do not copy its full rule set.
- Preserve the important local expectations: direct implementations, no compatibility shims or feature flags unless requested, avoid broad `codex-core` growth, use focused tests, keep Flowdex documentation current, preserve unrelated changes, and commit worker-owned changes with a brief summary.
- Correct the high-level plan's execution decision: Flowdex uses the existing native V8 code-mode runtime and does not require Node 22.
- Rename Flowdex-owned stored context objects to **context chunks** in the high-level plan. Codex's existing `context-fragments` remains the final prompt-injection mechanism.
- Record that `compact_context` is a required future Flowdex tool and is not assumed to exist upstream.
- Create `groundwork.md` as the implementation source of truth for this batch: explain the saved-file execution path, tool input/output, bootstrap object, important code locations, and what is deliberately not implemented yet.

Acceptance criteria:

- The fork has short, relevant agent instructions.
- The high-level plan no longer claims Node is required and consistently uses “context chunks” for Flowdex storage.
- The groundwork documentation is sufficient to write or inspect a basic workflow without reading the implementation plan.

Commit subject: `docs: establish Flowdex groundwork guidance`

### Task 2: Workflow source loader

Agent: `implementation_worker`

Depends on: Task 1 only for the updated terminology; it may begin after Task 1 commits.

Scope:

- Read: `codex-rs/code-mode-protocol/`, `codex-rs/code-mode/src/service.rs`, `codex-rs/utils/absolute-path/`, relevant workspace manifest and Bazel patterns.
- Write: new `codex-rs/flowdex/` crate, `codex-rs/Cargo.toml`, and the corresponding workspace/Bazel declarations owned by the new crate.
- Do not edit `codex-rs/core/`.

Instructions:

- Add a small `codex-flowdex` library crate. Keep it independent of `codex-core` and code-mode runtime state.
- Own only the stable file boundary:
  - Validate and load a workflow beneath `.flowdex/workflows/`.
  - Retain its canonical host path and repository-relative display path using appropriate path types.
  - Build the executable module source by prepending the `flowdex` bootstrap with serialized input/path values.
- Define a focused error enum that maps cleanly to tool errors. Do not add scheduler, agent, database, configuration, or review types.
- Write the narrow tests first and confirm they fail before implementation. Cover a valid workflow/bootstrap and one table-driven invalid-path test containing the important escape/non-`.js` cases.
- Follow existing crate and Bazel layout. If manifests or dependencies change, refresh the required lockfiles.

Acceptance criteria:

- Valid repository workflows load without exposing their source through an API response.
- Path escape and wrong-extension inputs are rejected before execution.
- Bootstrap values are produced with JSON serialization rather than manual escaping.
- The crate has no dependency on `codex-core` and contains no future scheduler placeholders.

Verification:

- Run the focused `codex-flowdex` tests.

Commit subject: `feat(flowdex): add repository workflow loader`

### Task 3: Start-workflow tool and native execution

Agent: `implementation_worker`

Depends on: Task 2.

Scope:

- Read: `codex-rs/core/src/tools/code_mode/`, tool registry/spec patterns, `codex-rs/core/tests/suite/code_mode.rs`, `codex-rs/tools/`.
- Write: the smallest new Flowdex tool module under `codex-rs/core/src/tools/`, necessary core manifest/Bazel entries, tool registration, and a sibling Flowdex integration test file.
- Do not implement scheduler, persistence, context chunks, agent orchestration, progress summaries, or Flowdex configuration.

Instructions:

- Register `start_flowdex_workflow` as a direct model tool with only `path` and optional `input` fields.
- Reuse `CodeModeService`, the current turn's tool definitions, dispatch broker, response adaptation, and output limits. Factor a small shared helper out of the existing code-mode handler only if needed to avoid duplicating that setup.
- Load and bootstrap the workflow through `codex-flowdex`, then start it as a normal code-mode cell.
- Map `RuntimeResponse` to the Flowdex output contract while preserving the cell ID as `runId`.
- Ensure the start tool cannot recursively appear in the workflow's nested tool set.
- Do not create a second runtime registry, background process, polling loop, or Node invocation.
- Write the integration test first and confirm it fails before implementation. The test should have the model call the new tool on a saved workflow, pass JSON input, and assert the workflow executes in V8 and returns the expected run ID/status/output shape.

Acceptance criteria:

- A model turn can start a saved `.flowdex/workflows/*.js` file without reading the source into model context.
- The workflow receives `flowdex.input` and may use top-level `await` and existing code-mode helpers.
- Immediate completion and yielded execution both retain a usable code-mode cell/run ID.
- Existing `functions.exec` behavior remains unchanged.

Verification:

- Run the focused Flowdex integration test and relevant code-mode tests only.

Commit subject: `feat(flowdex): start saved workflows in code mode`

## Orchestrator Integration and Review

After the three commits:

1. Inspect the combined diff for accidental schema fields, duplicated code-mode lifecycle logic, or unrelated edits.
2. Run `just fmt` in `codex-rs`.
3. Run focused tests for `codex-flowdex` and the new core integration path.
4. Run `just fix` only for the changed crates, then do not rerun tests solely because formatting/fix ran.
5. Dispatch one final reviewer using `gpt-5.6-luna` at `xhigh`. Review the complete vertical slice, not each worker commit. Ask specifically whether the implementation reuses code mode correctly, keeps source out of model context, and introduces any unnecessary abstraction.
6. Resolve actionable findings with the owning worker or directly when the fix is trivial. Keep fixes in small follow-up commits.
7. Update `flowdex-plan/flowdex-documentation/groundwork.md` if implementation details changed during review.
8. Leave the orchestration worktree clean.
9. Send the planning task a concise message containing the completed commit list, verification results, important implementation decisions, and any facts that affect Plan 002. Then compact context at the batch boundary.

## Non-Goals for Batch 001

- Task, phase, dependency, or dynamic queue scheduling.
- Agent spawn/message/reuse primitives.
- Event-driven Flowdex waiting or user-steering handling.
- Verification, review, worktree, or commit attribution logic.
- Context chunks or context-gathering agents.
- SQLite persistence, Flowdex configuration, progress events, AST-grep rules, installer work, or `compact_context`.
- CLI workflow execution.

Do not add placeholders, feature flags, compatibility shims, or speculative public types for these later batches.
