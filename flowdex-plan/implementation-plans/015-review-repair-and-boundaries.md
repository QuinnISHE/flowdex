# Flowdex Implementation Plan 015: Review, Repair, and Boundaries

## Outcome

The executable workflow can repair failed task verification, run task- and phase-level reviews with any declared agent, route structured findings to the responsible task agent, and suspend at task, phase, or run boundaries without polling or intermediate orchestrator turns.

Review is one composition built from role-neutral agents. This batch must not introduce worker/reviewer role types or a paired-agent loop abstraction.

## Settled starting point

- `flowdex.startRun(...)` owns durable phases, dependency-ready parallel tasks, dynamic queues, exact-HEAD verification, deterministic integration, and scheduler-owned progress.
- Task operations already map agent threads to source commits; integration maps each source commit to its actual integrated commit, model, agent, and summary.
- `resumeAgent`, `sendMessage`, and `waitAgent` are role-neutral, event-driven saved-workflow primitives. Task-associated resume operations preserve the task worktree gate and commit attribution.
- Workflow children are ordinary Codex agents with app-server lifecycle events and StatusOnly parent delivery.
- `wait_flowdex_workflow` already waits without polling and wakes for user steering.
- Context packs and AST-grep checks continue through their accepted seams.

## Generic programmed rounds

Do not create a `Worker`, `Reviewer`, `Pair`, or `Round` runtime object. JavaScript is the general orchestration language:

```js
for (let round = 0; round < input.maxRounds; round++) {
  const first = await flowdex.resumeAgent(firstAgent, makeResearchPrompt(round), {
    contextMode: "keep",
  });
  const second = await flowdex.resumeAgent(secondAgent, makeCritiquePrompt(first), {
    contextMode: "keep",
  });

  if (workflowDefinedCondition(first, second)) {
    break;
  }

  await flowdex.sendMessage(firstAgent, makeFollowUp(second));
}
```

- Any agents may exchange messages or completed results in either direction.
- The saved workflow defines the participants, ordering, condition, and numeric budget with ordinary JavaScript.
- The runtime does not interpret agent names or require one participant to be a reviewer.
- Reusable repository/global workflows can package these loops and declare `maxRounds` as an ordinary input field.
- Existing `resumeAgent` result ownership and `sendMessage` delivery remain the source of truth. Do not add a redundant rounds helper merely to wrap a `for` loop.

Declarative task review below is a scheduler convenience for the common implementation-plan path. Its reviewer is still an ordinary declared agent, and its report/attribution tool is a separate capability rather than the general messaging mechanism.

## Final workflow definition additions

Add only these strict fields to the existing definitions:

```js
const run = await flowdex.startRun({
  name: "parser-update",
  boundary: "continue",
  agents: {
    implementer: { profile: "implementation_worker" },
    reviewer: { model: "gpt-5.6-luna", reasoningEffort: "xhigh" },
  },
  phases: [{
    name: "implementation",
    instructions: "Implement and commit the requested parser changes.",
    boundary: "continue",
    review: {
      agent: "reviewer",
      instructions: "Review the integrated phase changes.",
      maxRounds: 2,
    },
    tasks: [{
      name: "parser",
      agent: "implementer",
      instructions: "Update the parser.",
      verification: ["cargo test -p codex-parser"],
      verificationRepairLimit: 2,
      review: {
        agent: "reviewer",
        instructions: "Review this task's committed diff.",
        maxRounds: 2,
      },
      boundary: "continue",
    }],
  }],
});
```

- `boundary` is exactly `"continue"`, `"orchestrator"`, or `"human"`; omission means `"continue"`.
- `verificationRepairLimit` is a non-negative integer; omission means zero automatic verification repairs.
- `review` is optional and has exactly `agent`, `instructions`, and positive integer `maxRounds`.
- The review agent must exist in the workflow's `agents` map. It is not a special role.
- Dynamic tasks accept the same task fields and validation.
- A task verification failure returns to that task's declared `agent`; do not add a second repair-agent field.
- Verification failures and review findings consume separate counters. A review-requested repair must pass verification again, but consumes another review round only when the next review reports findings.
- On success, a scope follows its declared boundary. Budget exhaustion or an unattributed finding targets the orchestrator by default; a scope with `boundary: "human"` targets the human. Unresolved findings never auto-continue.

Do not add callbacks, a policy DSL, or a second scheduler definition.

## Reviewer report and attribution tool

A scheduler-dispatched review agent receives the original scope requirements, relevant verification result, and committed diff. During that review operation only, expose one model-callable reporting tool:

```js
report_flowdex_review({
  findings: [{
    file: "codex-rs/parser/src/lib.rs",
    lineStart: 42,
    lineEnd: 48,
    reason: "The encoded field order does not match the required layout.",
    ruleKey: "parser.field-order",
    astGrepSuitable: false,
  }],
});
```

- Strictly reject unknown fields.
- `file`, `reason`, and supplied `ruleKey` are non-empty after trimming.
- Lines are positive inclusive integers and `lineEnd >= lineStart`.
- `ruleKey` is optional; `astGrepSuitable` is required.
- An empty findings array is an explicit pass.
- Exactly one accepted report belongs to a review operation. Completion without a report is an error, never an implicit pass.
- The tool is visible only to the active review agent. It is unavailable to the parent orchestrator, ordinary agents, saved-workflow V8 code, and general code mode.
- Reports and errors are bounded. Reports are durable workflow data, not parent-model messages.

The tool does not make an agent a reviewer permanently. The same agent profile may participate in ordinary research or implementation turns without this tool.

## Task review and verification repair

1. Run the task agent and capture commits through the existing attributed operation path.
2. Verify the exact task `HEAD`.
3. On failure, if the verification repair budget remains, resume the same task-associated agent with only bounded command/rule failures and a request to repair and commit. Passing verification remains silent.
4. If configured, dispatch the declared review agent after verification passes but before final task integration.
5. Empty findings accept the task. Otherwise persist the report, send one bounded grouped repair instruction directly to the task agent, resume it, capture its new commits, verify again, and start the next review round.
6. Integrate only after verification and task review pass.

Rust owns this declared scheduler composition, but it must use the same role-neutral agent submission and messaging seams that generic JavaScript loops use.

## Phase review and commit routing

1. Preserve participating task worktrees and task-agent associations until phase verification, review, and boundary handling finish. Phases without review keep the existing eager cleanup behavior.
2. Run phase verification against the integration worktree, then dispatch the configured review agent against the phase requirements and integrated phase diff.
3. For each finding, blame the inclusive lines at the current integration `HEAD`. Map the last Flowdex integrated commit to the stored source commit, task operation, and agent.
4. If no line maps, fall back to the most recent Flowdex commit from this run that touched the file. If none maps, keep the finding unattributed and pause at the escalation boundary.
5. Group findings by attributed task agent and resume those agents directly. Independent task repairs may run concurrently; existing task gates and advisory write-scope scheduling remain authoritative.
6. Reverify repaired task heads, incrementally integrate only their new attributed commits, record the finding-to-repair mapping, rerun phase verification, and start the next phase-review round.
7. After phase acceptance and its boundary, clean retained task worktrees through the existing safe cleanup path.

Keep the data transformation explicit:

`finding lines -> integrated commit -> source commit -> task operation -> agent thread -> repair commits`

Do not route by agent name, reviewer prose, or a semantic classifier.

## Durable state

Extend the existing per-repository SQLite store with only:

- Review operations: run, scope kind/ID, round, reviewer thread, and state.
- Findings: stable order, file, inclusive lines, reason, optional rule key, AST-grep suitability, and resolved attribution fields.
- Resolutions: finding plus exact repair operation/source commits.
- Separate verification-repair and review-round counters on their owning scheduled scope.
- Pending boundary: run, scope, target, reason, and transition awaiting continuation.

Use the existing store and migrations. Do not add a second database, durable event log, semantic clustering, rule candidates, or generic metadata table.

## Boundary waiting and continuation

- A pending boundary suspends the scheduler controller without terminating the run.
- Extend the existing event-driven `wait_flowdex_workflow` result with a bounded boundary variant containing `runId`, scope kind/name, target, and reason. Do not poll.
- User steering remains a wake source and does not consume or cancel the boundary.
- Add one direct-model-only continuation tool with exactly `run_id`. It resumes only that run's current boundary and rejects stale, missing, terminal, or already-consumed boundaries.
- Human approval uses the same continuation seam after the user's next turn. Revision feedback leaves the boundary pending while the orchestrator messages agents or queues valid work, then explicitly continues.
- A successful task/phase boundary occurs before dependents or the next phase proceed. The run boundary occurs before terminal completion delivery.
- Boundary progress is scheduler-owned and UI-only; it does not enter model history.

Preserve `start_flowdex_workflow`, `wait_flowdex_workflow`, `run.wait()`, queue, seal, and low-level task APIs. Add final result variants and the continuation tool without creating a second wait lifecycle.

## Progress and app visibility

Add deterministic scheduler summaries for verification repair, review round, routed repair, and orchestrator/human boundary transitions. There is no workflow- or model-callable progress tool.

Review and repair turns use ordinary Codex child threads, preserving app-server lifecycle visibility. StatusOnly suppresses only automatic parent-model completion delivery.

## Implementation sequence

Use brief scoped commits and parallel implementation workers only after the shared contract is committed:

1. Add the strict definition/domain types and SQLite migration.
2. Then in parallel:
   - `codex-flowdex`: review/finding/resolution persistence, counters, Git line/file attribution, retained task worktrees, and incremental integration.
   - `codex-core`: review report capture/tool exposure plus event-driven boundary observation/continuation, without duplicating scheduler policy.
3. Assemble the scheduler pipeline: verification repair, task review, phase routing/repair, separate budgets, boundaries, progress, cancellation, and cleanup.
4. Update concise source-of-truth documentation with the final schema, reviewer report, boundary results, and examples for both implementation review and a non-review research-agent loop.

Workers are not alone in the codebase and must not revert accepted Batch 001-014 behavior. Reuse the existing Git lifecycle; do not build a parallel integration system.

## Focused verification

Add only focused regressions that demonstrate the old gap before implementing each cohesive behavior; do not create a schema-field matrix.

The completed path must prove:

- Two ordinary research agents can exchange results for multiple JavaScript-controlled rounds and stop on a workflow-defined condition without any reviewer/report semantics.
- Failed task verification silently resumes the task agent, consumes only its verification budget, commits a repair, and passes.
- Task review reports a structured finding, routes directly to the task agent without parent-model delivery, repairs, reverifies, and passes on a later review round.
- Phase review attributes by line then file, runs independent repairs concurrently where safe, incrementally integrates them, and records exact finding-to-repair mappings.
- Verification and review budgets remain independent; exhaustion and unattributed findings suspend at the correct boundary.
- Boundary and steering wakes are event-driven, independently preserved, and continuation is one-shot.
- Review/repair agents stay app-visible while progress and StatusOnly completion stay out of parent model history.
- Existing scheduler parallelism, context collection, AST-grep verification, cancellation, and clean integration still work in the representative run.

Use focused crate tests plus one cohesive core Flowdex integration test. Run formatting and checks for affected crates only, using the established Windows test-thread stack override. Do not run the full workspace suite unless a focused failure points outside this scope.

After the pipeline works, use exactly one `gpt-5.6-luna` `xhigh` cohesive reviewer. Fix concrete correctness issues and rerun affected checks; do not dispatch per-worker reviewers.

## Non-goals

- No agent role types, fixed pairings, or reviewer-owned general messaging loops.
- No redundant `rounds` helper around JavaScript control flow.
- No tool profiles or broad configuration defaults.
- No AST-grep candidate clustering, proposal generation, or rule writing.
- No generic named signals or generic event bus.
- No process-restart scheduler resumption.
- No GUI changes, compatibility shim, feature flag, placeholder, alternate JavaScript runtime, or Node dependency.

## Completion record

Copy this plan unchanged into the implementation worktree. Keep brief commits with summaries. Document exact schemas, result variants, state transitions, attribution fallback, retained-worktree behavior, focused verification, and next-plan constraints. Compact context at the batch boundary and message the planner when complete; do not design the next plan.
