# Flowdex Batch 020: Boundary and context hardening

## Goal

Fix the two defects exposed by the video demo without adding new workflow concepts:

1. An orchestrator or human boundary must not let the saved JavaScript cell finish and cancel its scheduler before continuation.
2. Context-fragment freshness must treat LF and CRLF representations of the same text as identical.

## Boundary lifecycle

- Keep `wait_flowdex_workflow` as the model-facing event source for boundaries, steering, signals, and terminal state.
- Make saved-workflow `await run.wait()` wait for scheduler terminal completion only. It must remain pending while an orchestrator or human boundary awaits `continue_flowdex_workflow`.
- Preserve the existing direct boundary result and one-shot continuation contracts.
- Remove the timing workaround from the boundary integration fixture. Prove a normal workflow using `flowdex.output(await run.wait())` reaches a boundary, continues, and completes without a sleep.
- Update the workflow documentation and skill example so the ownership split is explicit: JavaScript waits for terminal state; the orchestration model handles boundaries through direct tools.
- Do not add a second wait API, retry loop, grace timer, compatibility shim, or scheduler lifecycle.

## Context freshness

- Treat context fragments as UTF-8 logical text. Canonicalize CRLF and lone CR to LF before hashing or comparing freshness.
- Preserve every other byte of text; do not trim, re-indent, or normalize other whitespace.
- New publications store canonical content and its canonical hash.
- Existing rows must recover without a database migration: compare canonicalized stored content with canonicalized current source content when resolving freshness instead of trusting only a historical raw-byte hash.
- Keep source paths, line ranges, versioning, supersession, safe-handle reads, and stale-source attribution unchanged.
- Make a failed refresh identify the affected pack and, when available, the stale fragment key/path/range instead of returning only `context collector completed without fresh pack ...`.
- Add one focused regression covering an LF integration source and an equivalent CRLF collector/worktree source.

## Verification

- Run the focused Flowdex crate tests for context fragments.
- Run the focused core boundary continuation integration test with its existing Windows stack setting if required.
- Run `cargo check -p codex-flowdex -p codex-core` and formatting for touched files.
- Do not run a full workspace build or release compilation.

## Completion

- Commit the plan and implementation in brief scoped commits.
- Keep unrelated files untouched and leave the checkout clean.
- Report the exact behavior change, focused evidence, and any remaining limitation to the planner task.
