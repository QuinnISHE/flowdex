# Flowdex video demo

## Story

Ask Flowdex to finish a small incident-digest crate. The workflow researches the contract, gathers reusable context, implements independent parser and renderer tasks in parallel worktrees, adds documentation dynamically, verifies and reviews the integrated result, then waits for human approval.

## Prepare

1. Build the modified `codex` binary.
2. Copy the release binary into `flowdex-package` as `flowdex.exe` or `flowdex`, run `flowdex install`, and fully restart the Codex app.
3. Record from a clean worktree on the `codex/flowdex` branch so the intentionally incomplete demo crate is visible.

## Start prompt

```text
Use $run-flowdex-workflows to start repo:demo/incident-digest with this input:
{"request":"Finish the incident digest crate exactly as specified"}

Wait event-first. When the workflow signals research-complete, summarize what completed and wait again. At the human boundary, show me the result and ask before continuing.
```

## Recording beats

1. Show `.flowdex/workflows/demo/incident-digest.js`: strict input, a nested reusable workflow, one signal, a context pack, two independent tasks, a dynamically queued dependent task, verification, review, and a human boundary.
2. Start the workflow. Point out the two research agents in the app. Their messages pass directly between agents; the orchestrator is not polling them.
3. When `research-complete` wakes the orchestrator, explain that the named signal carried no payload and did not add a model turn when published.
4. Wait again. The app's reasoning-summary area automatically shows scheduler state such as context collection, task execution, verification, and review. Those summaries are not model-callable and do not enter model history.
5. Show the context collector publish focused fragments from `SPEC.md` and the tests. The parser and renderer receive the pack without sending its contents through the orchestrator.
6. Show `parse-input` and `render-digest` running concurrently in separate worktrees. Each agent commits a brief summary; integration remains deterministic.
7. Show `document-result` begin only after both implementation tasks. It was queued after the run started.
8. Let silent verification pass. If review finds a defect, show the line finding route back to the attributed agent and the separate review-round budget.
9. At the human boundary, inspect the diff and commits. Send a normal steering message while the workflow is suspended to show that user input still wakes the task without consuming the boundary.
10. Approve with `continue_flowdex_workflow`. Wait once more and show the terminal result.

## Useful cutaways

- Open `.codex/skills/` to show the short model-facing guides for workflow dispatch, context publication, and review reporting.
- Open `.flowdex/workflows/defaults/worker-reviewer.js` and `research-rounds.js` to show reusable repository workflows with declared inputs.
- Show `resumeAgent` choosing `keep`, `compact`, or a fresh-thread `handoff` when a later workflow reuses an agent.
- Edit a published source range and rerun the demo to show stale context being recollected and superseded without blocking unrelated ready tasks.
- Run `scan_flowdex_rule_candidates({})` after repeated resolved findings. The scan is read-only; writing an AST-grep rule still requires explicit human approval.
- Show `compact_context({})` at a natural boundary and the configurable token reminder.
- Mention that tool profiles can overlay web, MCP, and existing tool configuration per declared workflow agent.
- Show the same installer command on Windows or macOS; it updates only the platform's Codex app backend setting.

## What the demo proves

| Beat | Flowdex capability |
| --- | --- |
| Research exchange | Generic agent messaging and bounded role-neutral rounds |
| Nested call | Reusable repo/global workflows with strict JSON input/output |
| Signal and wait | Event-driven suspension that preserves user steering |
| Context collection | Durable, refreshable context packs outside orchestrator history |
| Parallel implementation | Dependency scheduling, advisory scopes, task worktrees, commit attribution |
| Dynamic documentation | Queueing a task into an open phase, then sealing it |
| Verification and review | Silent commands, separate repair/review budgets, attributed findings |
| Human boundary | Explicit approval before the scheduler continues |
| App display | Automatic nonpersistent progress and normal subagent lifecycle events |
