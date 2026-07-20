# Flowdex Implementation Plan 019: Multi-Agent Version Override

## Outcome

Add one optional Flowdex-owned setting that selects the Codex multi-agent backend for every model in a newly loaded Codex runtime:

```toml
# $CODEX_HOME/flowdex.toml or trusted-repository .flowdex/config.toml
multi_agent_version = "v1"
```

The only accepted values are `"v1"` and `"v2"`. When the field is absent, Codex keeps its existing model-catalog and feature-based behavior. This setting lives only in Flowdex configuration; do not add a new field to Codex `config.toml`.

## Data transformation

Implement the feature as one direct path:

1. Parse the optional TOML string into a small typed Flowdex enum.
2. Merge it through the existing global-first, trusted-repository-override Flowdex configuration loader.
3. Carry the resolved optional value in `FlowdexConfig`, which is already attached to Codex `Config`.
4. Map the Flowdex enum to Codex's existing `MultiAgentVersion` at the core boundary.
5. Consult that value before model metadata or the existing `multi_agent_v2` feature when resolving a thread's backend.
6. Let the existing parent-to-child inheritance and app-server event paths carry the selected backend; do not add a second agent lifecycle.

This makes `v1` usable with any selected workflow model, including Luna or Codex Spark, because the existing V1 spawn path does not apply the V2-only compatibility filter. Selecting `v2` retains the existing V2 compatibility checks.

## Exact behavior

- Global source: `$CODEX_HOME/flowdex.toml`.
- Trusted repository override: `<repo>/.flowdex/config.toml`.
- A present repository value replaces the global value. An omitted repository value retains the global value.
- `"v1"` and `"v2"` are the complete vocabulary. Other strings fail configuration loading with the existing path-specific Flowdex error.
- An absent value means no override and preserves current Codex behavior exactly.
- The override applies to ordinary Codex agents and Flowdex workflow agents, not only to saved workflow execution.
- The override wins over per-model catalog metadata and the existing `multi_agent_v2` feature selection.
- It does not turn multi-agent support on when agents are otherwise unavailable or bypass existing collaboration availability, depth, or capacity limits.
- Subagents continue to inherit their parent's selected backend. No cross-backend parent/child runtime is introduced.
- Configuration remains load-once. A changed value takes effect in newly loaded tasks/runtimes; no hot reload is added.

## Implementation

### 1. Extend Flowdex configuration

In `codex-rs/flowdex/src/config.rs`:

- Add a public `FlowdexMultiAgentVersion` enum with strict lowercase serde names `v1` and `v2`.
- Add `pub multi_agent_version: Option<FlowdexMultiAgentVersion>` to `FlowdexConfig` and the partial TOML shape.
- Merge it with the same global/repository precedence already used by the other Flowdex settings.
- Keep the setting optional; do not invent `auto`, `default`, enable flags, per-agent variants, or model maps.

Re-export the enum from `codex-rs/flowdex/src/lib.rs` if core needs the public type.

### 2. Apply it at the existing Codex resolution seam

In `codex-rs/core/src/config/mod.rs`, map the resolved Flowdex value inside the existing multi-agent-version resolution functions. Keep one source of truth so root turns, spawned agents, resumed operations, Flowdex scheduler agents, reviewers, context collectors, and handoff agents all observe the same selected backend through their normal config/inheritance paths.

Do not modify model cache files, bundled model metadata, the remote model catalog, `spawn_agent` schemas, or app-server event types. Do not special-case Luna, Spark, or any other model.

### 3. Documentation and installed defaults

Document the field in the existing Flowdex configuration/getting-started source of truth with both values and the restart/new-task expectation. If the current installer-owned default `flowdex.toml` template is present in the checkout, include a concise commented example without changing its ownership or overwriting existing user configuration.

Preserve concurrent installer/default-workflow edits and stage only files owned by this change.

## Focused evidence

Add only the small owning assertions:

- Flowdex config parsing accepts `v1` and `v2`, rejects another value, and proves global/repository omission and replacement behavior.
- Core resolution proves Flowdex `v1` overrides a V2-tagged model and Flowdex `v2` overrides a V1-tagged model while absence preserves existing resolution.
- Existing unavailable-agent behavior remains unavailable.

Do not compile, run Cargo tests, run `cargo check`, or build the binary in this implementation task. Quinn will perform the real compilation afterward. Only run formatting for touched Rust files and `git diff --check`; report the focused tests that were added but not executed.

## Execution

The Sol/low orchestration task may use predefined `implementation_worker` or `implementation_worker_fast` subagents with standalone prompts and disjoint file ownership. Keep the implementation direct, preserve unrelated dirty files, and make brief scoped commits. One final self-review of the cohesive diff is enough; do not dispatch a separate reviewer or expand this into general model-catalog configuration.

If blocked, immediately notify the planner task rather than silently stopping. On completion, message the planner with the commits, exact configuration behavior, files changed, formatting/diff evidence, and any compilation caveat. Do not wait or poll for a reply.
