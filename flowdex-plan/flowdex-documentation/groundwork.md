# Flowdex Batch 001: Groundwork

This document is the source of truth for the first Flowdex vertical slice: a saved JavaScript workflow is loaded and executed by Codex's existing native V8 code-mode runtime. Event-driven orchestration waits are documented in [waiting.md](waiting.md).

## Starting a workflow

The model calls `start_flowdex_workflow` with exactly these inputs:

- `path`: a repository-relative `.js` file beneath `.flowdex/workflows/`.
- `input`: an optional JSON value; omitted input is exposed as `null`.

The tool reads the saved file without placing its source in model-visible output. It returns:

- `runId`: the underlying code-mode cell ID.
- `status`: `completed`, `yielded`, `failed`, or `terminated`.
- `output`: bounded output emitted before the returned state.
- `error`: included only for a JavaScript execution error.

The start tool is a direct model tool and is excluded from the workflow's nested tool set, so a workflow cannot recursively start itself.

## Workflow environment

The loader prepends a frozen bootstrap object to the saved source:

```js
const flowdex = Object.freeze({
  input: /* JSON input or null */,
  workflowPath: /* repository-relative path */,
});
```

The source executes as an async module. It may use top-level `await`, existing code-mode output helpers, and the nested `tools` object. The code-mode cell ID remains the run identifier across yielded and resumed execution.

## Safety and execution seam

The workflow path is canonicalized and checked against the canonical `.flowdex/workflows` directory before reading. Absolute paths, traversal, symlink escapes, missing files, and non-`.js` files are rejected with concise tool errors. Path handling uses Codex path types; source is not copied into conversation context.

Execution reuses `CodeModeService`, the current turn's nested-tool definitions, dispatch broker, response adaptation, and existing output limits. No Node process, second JavaScript engine, background polling loop, or separate cell lifecycle is introduced.

## Explicit non-goals

Batch 001 does not implement task or phase scheduling, dependencies or dynamic queues, agent spawning or messaging, verification/review loops, worktrees or commit attribution, persistence/configuration, context chunks or context gathering, progress events, installers, CLI workflow execution, or a `compact_context` Flowdex tool. See [waiting.md](waiting.md) for the later event-driven wait contract.
