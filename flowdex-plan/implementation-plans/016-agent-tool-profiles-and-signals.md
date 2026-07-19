# Flowdex Implementation Plan 016: Agent Tool Profiles and Named Signals

## Outcome

Finish the two remaining additive workflow-language controls without replacing the scheduler or agent lifecycle established in Batches 002, 009, 010, and 015:

- A workflow agent may select a named Flowdex tool profile in addition to its normal `.codex/agents` profile, model, and reasoning effort.
- Existing `.codex/agents` profiles provide defaults for Flowdex agents; explicit workflow model and reasoning settings take final precedence.
- A saved workflow may publish a named signal that wakes `wait_flowdex_workflow` event-driven, including when the signal was published just before the wait began.

This batch is additive. Keep the existing `startRun`, task, review, boundary, nested-workflow, and generic agent APIs. Do not create another agent-profile system, scheduler, wait lifecycle, or event framework.

## Settled Starting Point

Begin from authoritative Flowdex commit `e360b5fe6c37eb78a542743ea6e3f640df9d354b`.

The following behavior is already authoritative and must remain intact:

- `profile` resolves through Codex's normal `.codex/agents` configuration layers.
- Workflow agents already accept `profile`, `model`, and `reasoningEffort`, requiring at least one of those selectors.
- Task, review, repair, context-collector, handoff, and direct saved-workflow agents all use the ordinary Codex child-agent lifecycle and remain visible through app-server events.
- `wait_flowdex_workflow` already waits without polling, wakes for steering, messages, boundaries, and cell lifecycle, and does not consume user input.
- Boundaries and steering remain independent. A steer never implicitly cancels the workflow or consumes a pending boundary.
- Flowdex configuration loads once from `$CODEX_HOME/flowdex.toml`, then overlays an eligible trusted repository's `.flowdex/config.toml`.

## Final Agent Contract

Extend the existing agent selector object in place:

```js
agents: {
  researcher: {
    profile: "explorer",
    model: "gpt-5.6-luna",
    reasoningEffort: "high",
    toolProfile: "research",
  },
}
```

`toolProfile` is optional and does not count as a model selector. At least one of `profile`, `model`, or `reasoningEffort` remains required.

Add the same optional `toolProfile` field to the existing low-level `flowdex.spawnAgent(...)` and `task.runAgent(...)` specifications. Do not add it to `resumeAgent`: keep/compact reuse the resolved child configuration, and handoff inherits that complete resolved configuration.

Every scheduler-owned use of a declared agent must receive the same resolved tool profile:

- initial task execution;
- verification repair;
- task and phase review;
- attributed review repair;
- context-pack collection;
- handoff replacements.

Reject unknown fields and unknown tool-profile names before dispatching an agent. Keep the public casing `toolProfile` and the Rust/storage casing `tool_profile`.

### Configuration precedence

Resolve a Flowdex child configuration in this order:

1. Parent thread's resolved configuration snapshot.
2. Existing `.codex/agents` profile, when selected.
3. Selected Flowdex tool profile, when selected.
4. Explicit workflow `model` and `reasoningEffort` values, when present.

This ordering is Flowdex-specific. Do not change the ordinary Codex multi-agent profile semantics. The important result is that a Flowdex workflow can reuse an agent profile as defaults and deliberately override only model or reasoning effort.

## Tool Profile Configuration

Add a strict `tool_profiles` map to the existing Flowdex configuration. Merge global and trusted-repository maps by profile name; a repository profile replaces the same-named global profile as one complete definition, while unrelated global profiles remain available.

Use only existing Codex tool-related configuration types:

```toml
[tool_profiles.research]
web_search = "live"

[tool_profiles.research.tools.web_search]
context_size = "high"
allowed_domains = ["docs.rs", "github.com"]

[tool_profiles.research.mcp_servers.docs]
url = "https://docs.example.com/mcp"
enabled_tools = ["search", "open"]
disabled_tools = ["write"]
tool_timeout_sec = 30
```

Each profile may contain exactly:

- `web_search`, using the existing `WebSearchMode`;
- `tools`, using the existing `ToolsToml`;
- `mcp_servers`, using the existing named `McpServerConfig` map.

Apply those fields through the normal Codex configuration rebuild and tool-planning path. Do not add an arbitrary model-tool allowlist: Codex has no settled general allowlist seam, and inventing one here would be a separate feature. Do not allow a tool profile to set model, reasoning, provider, instructions, permissions, sandboxing, features, skills, plugins, agents, or arbitrary `ConfigToml` keys.

Configuration errors remain path-specific and fail before workflow execution. Missing configuration files remain normal. Preserve current trust gating and load-once behavior.

## Named Signal Contract

Expose one role-neutral saved-workflow primitive:

```js
await flowdex.signal("build-complete");
```

The method accepts exactly one non-empty signal name, returns exact JavaScript `undefined`, and is available only inside saved Flowdex JavaScript. It is not a direct model tool, ordinary `functions.exec` tool, general code-mode primitive, progress API, or agent tool.

`wait_flowdex_workflow({ run_id })` may now return:

```json
{ "runId": "cell-1", "status": "signal", "signal": "build-complete" }
```

The signal result intentionally wakes the orchestrator model. Publishing the signal must not itself call a model or append a message, progress item, conversation item, rollout item, or next-request input. The only model-visible representation is the eventual `wait_flowdex_workflow` tool result.

### Signal delivery

Persist pending signals in the existing repository Flowdex database as an ordered per-run FIFO. This prevents a signal emitted immediately before a wait subscription from being lost and preserves repeated signals with the same name.

Use a narrow runtime notification channel to wake active waiters, with SQLite remaining the source of pending delivery:

1. Subscribe before the final pending-state check.
2. Check and return the oldest pending signal before sleeping.
3. Consume exactly the signal that is returned.
4. If steering wins a simultaneous wake, return `steered` and leave every pending signal untouched.
5. Do not consume boundaries, messages, signals, or cell outcomes that were not returned.

Keep the existing bounded-error and cancellation behavior. A terminal workflow may still have an earlier queued signal returned first; the following wait observes terminal completion.

This is a purpose-built signal queue, not a generic event bus. Do not add payloads, external signal injection, selectors, routing, subscriptions, retention settings, background polling, or process-restart controller recovery.

## Implementation Shape

Settle the shared names and data shapes first, then use parallel workers where file ownership is clean:

1. **Shared contract:** add `toolProfile` normalization/domain fields, the strict tool-profile config type, and the exact signal/wait result shape.
2. **Tool-profile resolution:** apply the existing agent profile, then the selected tool-only config fragment, then explicit model/reasoning overrides across direct and scheduler-owned agent paths.
3. **Signal persistence:** add the minimal FIFO schema and store operations without changing existing run/task/review tables.
4. **Signal runtime:** add the hidden saved-workflow signal primitive and extend the existing event-driven wait with queued delivery and steering preservation.
5. **Assembly and documentation:** connect scheduler-owned agent uses, update the agent/config/waiting documentation, and remove stale statements claiming scheduler boundaries are absent.

Workers should commit their owned changes with brief summaries. Integrate completed worker commits promptly onto the batch line; do not leave the final assembly until several divergent worktrees have accumulated.

## Focused Verification

Verification exists to prove the new data transformations and the existing wake guarantees, not to exhaustively test every TOML or schema field.

- A global tool profile loads; a trusted repository replaces the same-named profile and preserves unrelated global profiles.
- A disallowed non-tool key is rejected.
- A Flowdex agent receives the selected tool settings, while explicit model/reasoning override values from its `.codex/agents` profile.
- Task, review, and context-collector dispatch use the declared agent's same resolved tool profile; handoff retains it.
- A signal emitted before a wait is delivered once.
- Repeated same-name signals are delivered FIFO without coalescing.
- Steering wakes first without consuming a queued signal, and the next wait receives that signal.
- Signal publication causes no model call or history/progress leakage.
- Existing scheduler, review/boundary, context-pack, and ordinary timed-wait focused cases still pass.

Run focused `codex-flowdex` unit tests, the affected `codex-core` wait/config tests, and one cohesive saved-workflow integration path. Use the established Windows test-thread stack override where the existing harness requires it. Do not run the full workspace suite unless focused evidence identifies a broader risk.

After the complete path works, use exactly one `gpt-5.6-luna` `xhigh` reviewer for the cohesive change. Fix concrete findings and rerun only affected checks.

## Documentation

Update the minimal source-of-truth documentation to show:

- the final agent selector and tool-profile TOML;
- profile/tool-profile/explicit override precedence;
- which scheduler-owned agents inherit a tool profile;
- `flowdex.signal(name)` and the exact wait result;
- the fact that signals wake the orchestrator but do not invoke it when published;
- generic researcher/researcher or other multi-agent rounds remain ordinary JavaScript loops and are unrelated to tool profiles or signals.

## Non-goals

- A second agent-profile registry or runtime role enum.
- A new core-tool allow/deny framework.
- Arbitrary raw Codex configuration inside a tool profile.
- Reconfiguring an existing agent during `resumeAgent`.
- Signal payloads or external producers.
- A generic event bus, generic wait selector, polling loop, or durable event-history API.
- Process-restart scheduler/controller resumption or orphan cleanup.
- AST-grep candidate promotion; that is the next feature slice built on Batch 015 review history.
- Compatibility shims, feature flags, placeholders, Node, or a second JavaScript runtime.
