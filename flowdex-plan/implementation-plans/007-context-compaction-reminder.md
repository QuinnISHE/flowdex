# Flowdex Implementation Plan 007: Context Compaction Reminder

## Outcome

When a Codex thread's active context reaches the Flowdex reminder threshold, the next model inference receives one developer reminder to call `compact_context` at the next natural task boundary. The reminder does not wake the model on its own and is not repeated within the same context window.

This batch also adds the smallest useful Flowdex configuration surface: one global setting with an optional repository override for that threshold.

## Why this is the next slice

Batch 006 added the model-controlled compaction action and deliberately deferred the token reminder. Codex already exposes the remaining data and lifecycle seams:

- active context usage is calculated from session token accounting;
- every compaction window has an existing stable window ID;
- the turn loop has a pre-inference point for recording developer context;
- `ContextualUserFragment` already produces developer-role model input; and
- configuration loading already resolves the Codex home, current project root, and project trust.

The required transformation is:

```text
$CODEX_HOME/flowdex.toml + <project>/.flowdex/config.toml
  -> resolve one threshold, with the project value winning
  -> before an inference, compare active context usage to the threshold
  -> if this window has not been reminded, record one developer message
  -> the next inference sees the reminder
```

Do not introduce a general notification framework or a full future Flowdex configuration schema in this batch.

## Configuration contract

Support one setting:

```toml
compaction_reminder_threshold_tokens = 150000
```

Load it from:

1. `$CODEX_HOME/flowdex.toml`;
2. `<project-root>/.flowdex/config.toml`, which overrides the global value when present.

Use the Git repository/worktree root already resolved by Codex. When there is no Git root, use the resolved session working directory, matching Codex's existing active-project fallback.

The default is `150000` when neither file sets the field. The value must be a positive integer. Missing files are normal; malformed files, unknown fields, and invalid values should produce a path-specific configuration error instead of silently changing behavior.

Use Codex's existing project trust decision for the repository file. Do not invent another trust store or prompt. The global file is always eligible; overlay the repository file only where Codex already permits repository-owned configuration.

There is no enabled flag, message-template field, percentage mode, hot reload, environment-variable override, or per-agent override in this batch.

## Reminder contract

At or above the resolved threshold, record this concise developer guidance before the next inference:

```text
Your context window is growing. At the next natural task boundary, call compact_context.
```

- Use active context tokens, not rollout-budget usage, weighted usage, remaining-token estimates, or the auto-compaction scope counter.
- Deliver at most once for each existing compaction window ID.
- Do not start an inference solely to deliver the reminder. If a response finishes without a follow-up, delivery waits for the next ordinary user turn.
- If manual or automatic compaction occurs before the next inference, evaluate the new window and its new token usage; do not deliver a stale reminder for the old window.
- The reminder is model-visible developer context and follows the normal conversation recording/persistence path. It is not a reasoning-summary event and should not appear as a user message.
- A resumed session may remind again if its reconstructed active window is already above the threshold. Do not add durable reminder bookkeeping merely to suppress that harmless repeat across process lifetimes.
- Existing automatic compaction thresholds and behavior remain unchanged.

## Implementation work

### 1. Add the narrow Flowdex config loader

Keep Flowdex-owned parsing in `codex-flowdex`, independent of `codex-core`.

Add a small config module that:

1. starts with the `150000` default;
2. reads the optional global partial config;
3. overlays the optional trusted repository partial config;
4. validates the resolved positive threshold; and
5. returns a resolved `FlowdexConfig` containing only `compaction_reminder_threshold_tokens`.

Use optional fields only in the private partial-file representation so an omitted repository field preserves the global value. Do not build a generic recursive merge layer for one scalar.

Load the resolved value while Codex configuration already has `codex_home`, resolved cwd, repository root, and trust available, then carry the resolved value with the session configuration. Do not reread either file before every inference.

### 2. Track delivery per context window

Add a tiny in-memory session state holding the last window ID for which the compaction reminder was recorded.

The decision should take:

- current window ID;
- current active context token count; and
- resolved threshold.

It returns due only when the count is at or above the threshold and that window has not already been recorded. Mark the window after the reminder is recorded. Do not couple this state to `PendingContextAction` or change compaction-window advancement.

### 3. Record the reminder before inference

At the existing pre-inference turn-loop boundary, obtain the current window ID and active context usage, check the reminder state, and record one developer-role contextual item when due.

Reuse the established contextual developer-message path used by other runtime reminders. Keep the check before prompt history is cloned so the next request contains the new item. Do not add a synthetic user turn, tool call, UI event, or extra sampling request.

The reminder check should naturally run again after compaction because the existing loop observes the new window ID and recomputed token usage.

### 4. Update documentation

Extend `flowdex-plan/flowdex-documentation/compaction.md` and the nearest configuration overview or index with:

- the exact global and repository paths;
- the one supported TOML field and precedence;
- the `150000` default;
- once-per-window delivery behavior; and
- the distinction between the reminder and automatic compaction.

Remove the statement that threshold reminders are future work.

## Task decomposition

1. Use one `implementation_worker` for the `codex-flowdex` configuration loader and its focused unit coverage. It owns the Flowdex crate and dependency metadata and commits its work with a brief summary.
2. After that API is committed, use one `implementation_worker` for core configuration integration, reminder state, pre-inference delivery, and the cohesive integration test. It must consume the settled loader API rather than redesign it and commit its work separately.
3. After behavior is stable, use one `implementation_worker_fast` for the Flowdex documentation update and links. It must not rewrite implementation code.

Workers are not alone in the repository. They must preserve unrelated changes and must not revert another worker's commits.

## Focused verification

Add only coverage for the new behavior:

1. A compact `codex-flowdex` loader test covers the default, global value, repository override, and rejection of an invalid value. A table-driven test is sufficient.
2. One cohesive core integration starts below the threshold, crosses it through reported model usage, proves the next request contains exactly one developer reminder, and proves another follow-up in the same window does not append a duplicate.
3. If the cohesive test does not exercise the actual configuration seam, add one small configuration test proving repository precedence. Do not duplicate general Codex config-layer tests.

Run formatting, scoped fixes/checks, `cargo test -p codex-flowdex`, the focused new core test, and directly affected existing compaction tests. A full workspace suite is not required.

## Final review

After the cohesive path passes, run one `gpt-5.6-luna` reviewer at `xhigh` effort. Ask it to review only:

- global/default/repository precedence and existing project trust;
- active-context accounting versus unrelated budget counters;
- once-per-window delivery and ordering before prompt cloning;
- manual and automatic compaction interactions;
- absence of an extra inference or user-visible message; and
- unchanged automatic compaction and `compact_context` behavior.

Fix actionable findings, rerun only affected focused verification, and keep the worktree clean.

## Explicit non-goals

This batch does not add:

- more Flowdex configuration fields or a generic schema/merge framework;
- automatic compaction policy changes;
- custom reminder text, percentage thresholds, or disabling controls;
- agent reuse modes (`keep`, `compact`, or `handoff`);
- a JavaScript compaction primitive;
- task/phase scheduling, dynamic queues, boundaries, or named signals;
- SQLite, durable run state, worktrees, or commit attribution;
- context chunks, review reporting, AST-grep rules, or installer work;
- a feature flag, compatibility shim, or placeholder API.

## Completion report

When Batch 007 is complete, report:

- commits and clean-worktree status;
- exact configuration paths, field, default, precedence, and trust behavior;
- the exact reminder text and delivery/persistence behavior;
- where once-per-window state is stored and evaluated;
- focused verification and reviewer findings; and
- any settled constraints Plan 008 must respect.

Copy this plan into the implementation worktree unchanged and verify its SHA-256 before implementation. Do not design Plan 008 in the implementation task. Compact the orchestration thread at the batch boundary, then send the completion report to the planning thread without polling it for a response.
