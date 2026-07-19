# Flowdex Implementation Plan 013: Context Packs

## Outcome

Flowdex workflows can name reusable context packs, have explorer agents populate them without returning source context through the orchestrator model, inject fresh context into dependent task prompts, and refresh fragments when their source changes.

This is the complete context-pack slice. Do not land a storage-only API that is intended to be replaced by the scheduler integration later.

## Public contract

Extend the settled `flowdex.startRun(...)` definition with strict `contextPacks` and task `context` fields:

```js
const run = await flowdex.startRun({
  name: "update-contact-layout",
  agents: {
    explorer: { profile: "explorer" },
    implementer: { profile: "implementation_worker" },
  },
  contextPacks: {
    "contact-manifold": {
      agent: "explorer",
      instructions: "Collect the current contact-manifold layout and invariants.",
    },
  },
  phases: [{
    name: "implementation",
    instructions: "Preserve the current binary layout.",
    tasks: [{
      name: "update-reader",
      agent: "implementer",
      instructions: "Update the reader.",
      context: ["contact-manifold"],
    }],
  }],
});
```

- `contextPacks` is an optional map. Each entry accepts exactly `agent` and `instructions`; both are required non-empty strings and the agent must exist in the run's settled agent map.
- A task's optional `context` is an array of unique declared pack names. Dynamic tasks use the same validation.
- Phase inheritance and all existing task fields remain unchanged.

Add a model-facing context publishing tool for explorer and task agents:

```text
publish_flowdex_context({
  pack,
  key,
  path,
  line_start,
  line_end,
  summary?
}) -> { pack, key, version }
```

The current Flowdex child/run association supplies repository and execution identity; callers do not provide database paths, run IDs, task IDs, or worktree roots. Publishing the same `pack` and `key` creates a new immutable version that supersedes the previous version.

Add a direct-model read tool for optional inspection:

```text
read_flowdex_context({ pack }) -> {
  pack,
  status: "fresh" | "missing" | "stale",
  fragments: [{ key, version, path, lineStart, lineEnd, summary?, content }]
}
```

Normal workflow execution does not call the orchestrator model or this read tool. Task injection reads the store internally.

## Data transformation

1. An explorer selects a repository-relative file and inclusive line range.
2. Flowdex validates the trusted repository and current execution worktree, reads the range from a single safe file handle, and stores the selected text plus its content hash as an immutable fragment version.
3. The latest version for each `(pack, key)` is the active pack view. Older versions remain attributable but are not injected.
4. Before a task starts, Flowdex re-reads every active source range from the current integration worktree. A missing file, changed range, or hash mismatch marks the pack stale.
5. Fresh fragments are formatted once in stable key order and appended to the task instructions. The orchestrator and workflow JavaScript never need to load the fragment content.
6. Publishing the same key after modifying its source supersedes the stale version. Do not attempt automatic line remapping across arbitrary edits.

Keep the aggregate injected/read result within the existing bounded-output conventions. If bounding occurs, mark the rendered context clearly rather than silently presenting it as complete.

## Missing and stale context

When a required pack is missing or stale, the scheduler must suspend only the blocked task and automatically spawn the pack's configured agent with:

- The pack name and collection instructions.
- The stale keys and source locations, when present.
- Instructions to publish one or more fresh fragments with `publish_flowdex_context`.

This collector is an ordinary app-visible StatusOnly Flowdex child using the existing agent capacity, model/profile resolution, cancellation, and event delivery. It is not a new explorer role or special agent runtime. Other dependency-ready tasks continue running.

Only one collection operation for a given run and pack should be active at once. After the collector completes, resolve the pack again and start every newly unblocked task. If the collector fails or finishes without producing a fresh pack, fail the affected scheduler path with a bounded diagnostic so the existing workflow wait wakes the orchestrator; do not loop indefinitely.

Emit scheduler-owned live progress such as `Collecting context: contact-manifold` and `Context ready: contact-manifold`. These remain non-persistent reasoning-summary events and must not invoke a model or enter model history.

## Persistence and ownership

- Add the smallest SQLite schema needed for pack declarations and immutable fragment versions to the existing per-repository Flowdex store.
- Record repository identity, run identity, publisher thread/agent when available, source path/range, content, hash, version, and superseded version.
- Keep trusted repository root separate from task/explorer execution cwd, following the established AST-grep and task-worktree pattern.
- Repository-relative paths are declarations resolved by native path types and safe-handle reads. Reject links/reparse escapes, directories, invalid ranges, and files outside the trusted repository.
- Fragment content belongs to Flowdex persistence and is not inserted into rollout history merely because it was published.

## Implementation shape

Use two implementation workers in parallel once the orchestrator has confirmed the final Rust types:

1. One worker owns the repository-independent context domain/store module and its focused store tests.
2. One worker owns the core context tools plus scheduler/task prompt integration, using the public domain contract and avoiding writes to the store worker's files.

The orchestration thread owns shared exports/registration, resolves the small integration seam, updates documentation, and runs the cohesive path. Workers are not alone in the codebase, must preserve concurrent edits, and must commit their scoped work with brief summaries.

## Verification

Keep verification proportional to the new failure modes:

- Store-focused coverage for immutable supersession and fresh/stale resolution.
- One cohesive core workflow covering missing pack -> automatic collector -> publish -> task prompt injection, then source modification -> superseding publish -> fresh reinjection.
- One path/trust case proving a fragment cannot read outside the trusted repository or through a link/reparse escape.
- The existing executable scheduler group to catch accidental scheduling regressions.
- Formatting and checks for `codex-flowdex` and `codex-core`.

After the complete path works, use one `gpt-5.6-luna` xhigh review focused on stale-context correctness, path/worktree identity, accidental model-history leakage, collector deduplication, cancellation, and scheduler regressions. Do not dispatch per-worker reviewers or create a broad test matrix.

## Non-goals

- No semantic embeddings, vector database, model-based fragment clustering, or automatic line remapping.
- No hardcoded explorer/worker/reviewer roles.
- No generic event bus or process-restart scheduler work.
- No compatibility alias for another context-fragment API.
- No context content routed through the orchestrator unless it explicitly calls the read tool.
