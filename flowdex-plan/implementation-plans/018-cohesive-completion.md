# Flowdex Implementation Plan 018: Cohesive Completion

## Outcome

Accept Flowdex as one joined system rather than seventeen isolated feature slices. Exercise a representative saved workflow through the real V8, scheduler, agent, worktree, verification, review, context, boundary, event, and completion paths; fix only defects that the joined path exposes; and leave one concise user-facing source of truth for building, installing, authoring, starting, observing, and extending Flowdex workflows.

This batch does not design a new API. The accepted APIs at `1305d0d08fb99bc1d308158b3df09a00289f1a64` are the final contract unless the cohesive path proves one is internally inconsistent.

## Starting point

Start from exact authoritative commit `1305d0d08fb99bc1d308158b3df09a00289f1a64`.

A clean detached worktree at that commit is expected and authorized. Do not attach or move `codex/flowdex`, merge `main`, or reconcile unrelated work. If the baseline differs, the worktree is dirty, or another hard blocker prevents progress, immediately message the planner task before stopping.

## Completion transformation

Treat the work as four direct transformations:

1. Map every requirement in `flowdex-plan/PLAN.md` to its accepted public contract, implementation seam, focused evidence, and user documentation.
2. Turn that map into one representative joined workflow scenario plus references to focused tests for behavior that would make the scenario timing-dependent or artificial.
3. Turn any observed joined-path failure into the smallest production fix at the existing seam, with a regression assertion in the same scenario or owning focused test.
4. Turn the final accepted contracts into a short entry guide and accurate linked reference pages. Do not duplicate every feature document into a second exhaustive manual.

## Settled contract

Preserve the accepted behavior from Batches 001-017:

- Saved and nested workflows run as ordinary asynchronous JavaScript in the existing native V8 runtime. Node is not a dependency.
- `flowdex.startRun(...)` remains the high-level run/phase/task language. Dependency-ready tasks run concurrently; advisory scope conflicts serialize only conflicting ready work; dynamic tasks enter open phases and phases finish only after sealing.
- Low-level agent, task, verification, context, rule, signal, and nested-workflow primitives remain additive capabilities. Do not replace them or add wrapper aliases.
- Workflow-spawned agents keep normal app-server lifecycle visibility while StatusOnly completion avoids waking or filling the orchestrator context.
- Automatic scheduler progress uses live reasoning-summary events only. It is not model-callable, persisted in rollout/conversation history, added to workflow results, or sent in the next model request.
- Verification and review remain separate. Verification repair and review rounds keep independent limits. Review alone has the structured attribution report; arbitrary agent-to-agent rounds remain ordinary JavaScript loops over role-neutral messaging and resume primitives.
- User steering wakes an outer event-driven wait without consuming queued signals, boundaries, or mailbox input and without cancelling the active workflow.
- Context packs are injected only into dependent tasks. Missing or stale packs dispatch one ordinary collector while unrelated ready tasks continue.
- Repository/global reusable workflows retain strict JSON input and exact JSON output, explicit scope, parent/child identity, cycle protection, cancellation, and event-driven waiting.
- Tool profiles remain tool-only overlays. Existing `.codex/agents` profiles and explicit model/reasoning precedence remain unchanged.
- AST-grep candidate scanning stays direct-model-only, read-only, bounded, and separate from explicit human-approved repository rule editing.
- `codex flowdex install --binary <absolute-path>` remains the Windows/macOS desktop-backend installer. A real macOS host acceptance run is desirable but unavailable here; do not pretend static tests are a live-host result.

## Work

### 1. Build the completion matrix

The orchestration agent should inspect `PLAN.md`, the documentation index, the accepted implementation plans, and the actual registered tool/workflow schemas. Record a compact completion matrix in the final documentation area with these columns:

- capability;
- user-facing entry point;
- source-of-truth document;
- focused evidence;
- intentional limit, if one matters to a user.

The matrix is an audit aid, not a new runtime manifest. Keep it maintainable and avoid file/line references that will immediately drift.

Before implementation, use the matrix to identify only concrete mismatches: missing behavior, stale documentation, or an accepted contract that is not reachable through the documented path. Do not promote optional follow-ups into blockers.

### 2. Exercise one joined workflow path

Add or extend one core integration scenario that starts a saved workflow through the existing model-facing entry point and demonstrates the joined scheduler path. Use the established Flowdex test harness and ordinary StatusOnly child responders; do not build a parallel fake scheduler.

The scenario should cover the interactions that benefit from being tested together:

- strict saved-workflow input reaches the workflow body;
- phase instructions reach task agents;
- two non-conflicting dependency-ready tasks are active concurrently and remain app-server-visible;
- a later task is queued while its phase is open, its dependency is honored, and the phase is sealed;
- task worktrees produce attributed commits and deterministic integration;
- a required context pack is collected or refreshed and injected only into its dependent task without blocking unrelated ready work;
- command verification and at least one repair/review transition use their existing independent accounting;
- an orchestrator or human boundary yields the existing boundary result, then the existing continue tool resumes the same run;
- automatic progress is emitted live without appearing in the parent model history or final workflow result;
- the run reaches its existing terminal completion result without an intermediate orchestrator inference.

Use existing focused tests rather than forcing the following into a brittle monster scenario: active-wait steering races, compact-context internals, generic two-agent research loops, nested-workflow cycle cases, installer platform mutation, rule-candidate history bounds, and every schema rejection. Confirm those focused tests still cover their owning contracts and add no duplicate cases merely for a checklist.

If direct agent messaging or a nested reusable workflow fits naturally in the saved module without timing tricks, include it. Otherwise, keep their existing focused evidence and show their composition in the authoring guide.

### 3. Fix only joined-path defects

When the scenario exposes a failure, repair the existing production seam directly. Preserve the settled public shapes unless the shape is genuinely impossible to use. Do not add:

- compatibility shims, feature flags, placeholders, aliases, or a second lifecycle;
- process-restart scheduler resurrection, orphan administration, or a generic event bus;
- a new workflow builder, task graph language, worker/reviewer role enum, or Node runtime;
- a callable progress tool, payload-bearing signal, candidate approval ledger, or automatic rule-writing agent;
- exhaustive validation tests unrelated to an observed gap.

Each production fix should be a small commit with a brief summary and the focused assertion that proves the defect is closed.

### 4. Finish the user-facing source of truth

Create one concise getting-started document and link it first from `flowdex-plan/flowdex-documentation/index.md`. It should let a user or future skill author follow the real path without reading implementation plans:

1. build the modified Codex CLI;
2. install it as the Codex desktop backend on Windows or macOS;
3. understand global and trusted-repository Flowdex locations;
4. save a repository or global workflow;
5. author one complete JavaScript example using strict input, reusable agents, a run, phases, dependency-ready tasks, context, verification/review, dynamic queuing/sealing, and a boundary;
6. start and event-wait the workflow from the model;
7. compose a named nested workflow and a small role-neutral agent messaging loop;
8. find the detailed contract for each advanced capability.

Keep the example executable against the accepted bootstrap and schema. Clearly label direct-model tools versus saved-workflow-only APIs. Explain that scheduler progress and child lifecycle are visible in the app without entering model history. Link to existing detailed pages instead of repeating their full contracts.

Correct stale names or statements discovered during the audit, including platform-neutral installer wording and any old callable-progress or polling language. Document optional limits honestly: controllers are live-process, macOS has no live-host acceptance result in this repository, scopes are advisory, and rule writing remains an explicit user-approved edit.

## Parallel execution

After the orchestration agent completes the shared completion matrix and identifies the joined scenario, use parallel implementation workers where ownership is disjoint:

- one `implementation_worker` owns the cohesive core/Flowdex scenario and any production defects it exposes;
- one `implementation_worker_fast` or `implementation_worker` owns the getting-started guide, index, and narrowly identified stale documentation;
- the orchestration agent owns integration, the matrix, cross-file contract consistency, and the final focused verification.

Workers are not alone in the codebase. They must preserve others' edits, commit brief scoped summaries, and message the orchestration agent immediately if blocked. Integrate completed worker commits promptly rather than leaving finished work stranded in child worktrees.

## Focused verification

Testing and review should demonstrate the joined behavior, not dominate the batch.

1. Before production changes, make the new joined scenario fail for the missing joined behavior it is intended to prove. If the audit finds no production gap, it is acceptable for the first change to be the new scenario and documentation; do not manufacture a red test.
2. Run the single joined Flowdex scenario with the established Windows test-thread stack when needed.
3. Run the existing focused Flowdex core group and `cargo test -p codex-flowdex`; do not run the full workspace suite unless a focused failure points outside these crates.
4. Run the focused CLI installer tests, including Windows/macOS pure/injected behavior, without touching the real registry or shell profiles.
5. Run `cargo check -p codex-flowdex -p codex-core -p codex-cli`, formatting for affected crates, and `git diff --check`.
6. Use exactly one `gpt-5.6-luna` xhigh reviewer after the joined path and documentation are complete. Ask it to review contract reachability, model-context isolation, app-server child visibility, boundary/steering preservation, worktree attribution, and documentation accuracy. Fix concrete findings, rerun affected focused checks, and do not dispatch a second reviewer unless the first identifies an unresolved high-risk defect.

## Acceptance criteria

- The completion matrix accounts for every requested capability in `PLAN.md` with no unexplained partial feature.
- A representative saved workflow crosses the real joined scheduler path and terminates successfully using existing public contracts.
- Workflow agents remain visible through normal app-server lifecycle events while automatic completion and progress remain absent from orchestrator model context/history.
- Parallel readiness, dynamic queuing, context preparation, verification/repair, review attribution, integration, and boundary continuation compose without deadlock, polling, or an extra model turn.
- The documentation index leads with a usable, schema-accurate end-to-end guide and links to the existing detailed references.
- Any remaining limits are explicit optional follow-ups, not silently missing requested behavior.
- No new public API, compatibility layer, feature flag, placeholder, alternate runtime, or speculative persistence is introduced.
- The final worktree is clean, and the completion report identifies the authoritative commit and the focused evidence used.
