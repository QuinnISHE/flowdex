# Flowdex Implementation Plan 006: Native Context Compaction

## Outcome

Codex models can call a native `compact_context` tool at a discrete work boundary. The current turn completes its tool call, Codex compacts that same thread through the existing compaction implementation, and the next model inference continues from the compacted history.

This batch adds the model-controlled action only. The token-usage reminder that encourages models to use it belongs in a later batch.

## Why this is the next slice

The settled Flowdex path can now start workflows, operate agents, verify work, report live progress, and suspend without polling. Long-running orchestrators and reusable agents still cannot deliberately compact their own thread.

Codex already owns the hard parts:

- local and remote compaction implementations;
- manual versus automatic compaction metadata;
- compact lifecycle events, hooks, persistence, and history replacement;
- mid-turn compaction between one model response and its follow-up inference;
- the session state and turn loop used by the existing `new_context` request.

The missing transformation is small:

```text
model tool call
  -> one pending compact request on this Session
  -> current response and tool output finish
  -> turn loop consumes the request once
  -> existing inline compaction replaces history
  -> next inference continues in the same turn
```

Do not invoke `Op::Compact` from the tool. That operation starts a standalone session task and is the wrong lifecycle for a tool called inside an active model turn.

## Public contract

Expose one direct model tool:

```text
compact_context({})
```

- The input schema has no fields and rejects additional properties.
- The description tells the model to call it at a natural task boundary when older detail is no longer needed.
- The tool returns a short acknowledgement that compaction is scheduled.
- Compaction happens before the next model inference, not in a later user turn.
- It compacts only the thread whose model called the tool. A sub-agent therefore compacts its own context, not its parent or sibling.
- It is available to ordinary Codex model turns, including sub-agents.
- It is `DirectModelOnly`: saved Flowdex JavaScript, ordinary `functions.exec`, and general code-mode nested tools do not receive it.
- It has no prompt, threshold, target-thread, strategy, or force fields.

The existing `/compact`/`Op::Compact` behavior and the existing `new_context` tool behavior remain unchanged.

## Implementation work

### 1. Represent the pending context action once

Extend the existing session request state used by `new_context` so it can distinguish:

- start a new context window; and
- compact the current context.

Prefer one small pending-action enum over independent booleans that can conflict. The request is one-shot and is taken only by the active turn loop.

If one model response somehow requests both actions, compaction wins because it preserves useful history. Consume the losing request at the same boundary so it cannot unexpectedly affect a later response.

Keep this state in memory with the active session. Do not add Flowdex run state or SQLite for it.

### 2. Add the direct model tool

Add a narrow handler and spec alongside the existing context-window tools.

The handler should:

1. accept only a normal function payload with `{}`;
2. mark the calling session's compact request;
3. return the short acknowledgement.

Register it as `ToolExposure::DirectModelOnly`. Do not gate it on collaboration or Flowdex workflow availability, and do not add it to the frozen `flowdex` JavaScript bootstrap.

### 3. Consume the request at the established turn boundary

In the post-sampling turn loop, take the pending context action only when the response needs a follow-up. For a compact request, run the same inline local/remote implementation selection already used by automatic mid-turn compaction, but record:

- `CompactionTrigger::Manual`;
- `CompactionReason::UserRequested`;
- `CompactionPhase::MidTurn`.

Generalize the existing inline compaction helpers just enough to accept the trigger instead of hard-coding `Auto`. Keep every current automatic caller passing `Auto` so its behavior and analytics do not change.

Preserve all existing compaction behavior:

- provider-specific local or remote selection;
- the active model client session where the existing path uses it;
- initial context/world-state injection;
- pre- and post-compact hooks;
- lifecycle items and persistence;
- cancellation and error reporting;
- token-budget mode's existing fresh-window compaction semantics.

After successful compaction, continue the same turn loop so the next inference sees compacted history. Do not create an extra model turn or inject a new user message.

### 4. Document the behavior

Add a short `flowdex-plan/flowdex-documentation/compaction.md` and link it from the existing Flowdex documentation index or nearest overview.

Document:

- the exact empty-argument call;
- that the calling thread is compacted;
- that execution resumes with the next inference;
- that workflows cannot call it from JavaScript;
- that the future threshold reminder is not part of this batch.

## Task decomposition

Use one `implementation_worker` for the cohesive Rust path. Its ownership includes session pending-action state, the tool handler/spec and registration, inline compaction trigger plumbing, and focused tests. It must commit that work with a brief summary.

After the API is stable, use one `implementation_worker_fast` for the documentation file and links. It must make its own documentation commit and must not rewrite the implementation worker's code.

Workers are not alone in the repository. They must preserve unrelated changes and must not revert another worker's commits.

## Focused verification

Add only coverage that protects this new seam:

1. A tool-spec test proves the exact empty schema and direct-model-only exposure.
2. One cohesive core integration proves a model can call `compact_context`, compaction occurs before the follow-up model request, and the next request uses the compacted history.
3. If the pending-action precedence is not obvious from the integration, add one small state test proving compact wins over `new_context` and is consumed once.

Reuse existing compaction fixtures and assertions. Do not duplicate the local/remote/manual compaction test matrix already owned by Codex.

Run formatting, scoped fixes/checks, the focused new tests, and the relevant existing compaction tests touched by helper signature changes. A full workspace suite is not required.

## Final review

After the cohesive path passes, run one `gpt-5.6-luna` reviewer at `xhigh` effort. Ask it to review only:

- request consumption and simultaneous-action precedence;
- manual metadata versus unchanged automatic metadata;
- history replacement before the next inference;
- hooks, cancellation, and error behavior;
- tool visibility and calling-thread isolation;
- regressions to `new_context` and standalone manual compaction.

Fix actionable findings, rerun affected focused verification, and keep the worktree clean.

## Explicit non-goals

This batch does not add:

- token thresholds, reminder injection, or Flowdex configuration;
- automatic compaction policy changes;
- agent reuse modes (`keep`, `compact`, or `handoff`);
- a JavaScript `flowdex.compact` primitive;
- task/phase scheduling, dynamic queues, or boundaries;
- SQLite, durable run state, worktrees, or commit attribution;
- context chunks, review reporting, AST-grep rules, or installer work;
- a feature flag, compatibility shim, or placeholder API.

## Completion report

When Batch 006 is complete, report:

- commits and clean-worktree status;
- the final tool schema and acknowledgement;
- where the pending request is stored and consumed;
- how manual trigger/reason/phase metadata is preserved;
- focused verification and reviewer findings;
- any settled constraints Plan 007 must respect.

Copy this plan into the implementation worktree unchanged and verify its SHA-256 before implementation. Do not design Plan 007 in the implementation task. Compact the orchestration thread at the batch boundary, then send the completion report to the planning thread without polling it for a response.
