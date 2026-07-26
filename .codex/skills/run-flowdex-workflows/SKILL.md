---
name: run-flowdex-workflows
description: Design, save, dispatch, and operate Flowdex JavaScript workflows. Use for implementation plans, parallel task graphs, reusable or nested workflows, context packs, automatic verification and repair, batched or task-level review, boundaries, signals, AST-grep checks, and bounded agent-to-agent rounds without keeping the orchestrator model awake.
---

# Run Flowdex Workflows

Author saved JavaScript for Flowdex's native V8 runtime. Do not require Node. Check `.flowdex/workflows` and the global workflow directory before creating a duplicate.

## Choose the shape

- Use `startRun` for implementation work requiring task worktrees, commits, dependencies, integration, verification, review, or boundaries.
- Use agent primitives directly for research, planning, debate, handoffs, and other role-neutral bounded conversations.
- Use `runWorkflow` for reusable behavior. Save project-specific modules as `repo:name` and portable modules as `global:name`.
- Use `createTask` only for a custom lifecycle that cannot use `startRun`; the scheduler is the normal implementation API.

Read the matching example before authoring:

- [`examples/implementation-run.js`](examples/implementation-run.js): parallel implementation, context, verification, batched phase review, and a boundary.
- [`examples/agent-rounds.js`](examples/agent-rounds.js): bounded role-neutral agent messaging.
- [`examples/reusable-workflow.js`](examples/reusable-workflow.js): strict input and exact JSON output for nesting.

## Workflow primitives

- `flowdex.input`: raw input object supplied at start.
- `flowdex.workflowPath`: resolved workflow reference.
- `flowdex.requireInput(schema)`: validate strict JSON input and return it. Declare this before doing work.
- `flowdex.output(value)`: return exactly one JSON-compatible value. Do not mix it with raw `text` or `emit` output.
- `await flowdex.runWorkflow("repo:name" | "global:name", input?)`: run a saved child workflow and receive its exact output.
- `await flowdex.startRun(definition)`: start the durable phase/task scheduler. The frozen handle exposes `id`, `queueTask`, `sealPhase`, and `wait`.
- `await flowdex.spawnAgent(spec)`: start a role-neutral child and return its ID.
- `await flowdex.sendMessage(agentId, message, { delivery? })`: use queued delivery by default. Prefer `resumeAgent` when the workflow must own the next operation and terminal result.
- `await flowdex.waitAgent(agentId)`: event-driven wait for the current operation's terminal status.
- `await flowdex.resumeAgent(agentId, instructions, { contextMode? })`: run the exact next operation with `keep`, `compact`, or `handoff` context.
- `await flowdex.verify(commands, { workdir?, timeoutMs? })`: run ordered commands silently, stopping at the first failure.
- `await flowdex.checkRules(ruleIds)`: run approved repository AST-grep rules explicitly.
- `await flowdex.signal(name)`: wake the outer model wait with a named, payload-free signal.
- `await flowdex.createTask(declaration)`: create a low-level isolated task handle with `id`, `runAgent`, `verify`, and `integrate`.

Progress summaries and child lifecycle events are automatic and app-visible. There is no callable progress primitive, and those events do not enter model history.

## Design runs

Put shared requirements and invariants in phase `instructions`. Put one concrete deliverable, relevant constraints, and completion criteria in each task `instructions`; do not repeat the phase text. Use `dependencies` only for semantic ordering. Dependency-ready tasks run concurrently. Declare `readScope` and `writeScope` to document ownership and avoid obvious write collisions; scopes are scheduling hints, not access controls.

Use multiple narrow tasks when they can produce independent commits. Keep tightly coupled edits in one task. Set `open: true` only when the workflow must discover work dynamically; queue tasks and always seal the phase.

Phases run in order. Put a boundary on the smallest scope requiring approval:

- `continue`: proceed automatically.
- `orchestrator`: wake the orchestration model for a decision.
- `human`: require explicit user continuation.

`run.wait()` waits for terminal scheduler completion and remains pending at orchestrator or human boundaries. The outer model observes boundaries with `wait_flowdex_workflow`, calls `continue_flowdex_workflow`, then waits again. Never poll.

## Configure agents

Every declared agent needs a `profile`, `model`, or `reasoningEffort`. Prefer a repository `.codex/agents` profile for stable project roles. Use an explicit model when the workflow must be portable without repository profiles. Explicit model and reasoning values override profile values.

- Use `gpt-5.6-sol` at `high` for substantial implementation.
- Use `gpt-5.6-terra` at `medium` or `high` for small, well-scoped edits when available.
- Use `gpt-5.6-luna` at `high` for exploration and `xhigh` for difficult final review.
- Reserve `xhigh` for judgment-heavy work; it is usually wasteful for mechanical tasks.

Use `toolProfile` for a named Flowdex tool overlay from `flowdex.toml`. Grant only the web, MCP, and existing Codex tools the role needs. A tool profile is separate from a `.codex/agents` profile.

## Use context packs

Declare a context pack when downstream tasks need exact source facts that the orchestrator should not load. Pack instructions should name the facts to collect, expected stable keys, and the smallest source ranges that prove them. Do not put implementation work or broad repository exploration into a pack.

Tasks opt in with `context: ["pack-name"]`. Flowdex injects fresh fragments only into those task prompts. Missing or stale packs dispatch the declared collector while unrelated ready tasks continue. Collectors publish bounded repository-relative inclusive ranges with `publish_flowdex_context`; republishing the same key supersedes its prior version.

## Configure verification

Use short, deterministic, non-interactive commands. Put checks at the scope whose state they validate:

- Task `verification` runs in that task worktree before integration.
- Phase `verification` runs after the phase's task commits integrate.
- Run `verification` runs against the final integrated worktree.

Flowdex tells task workers exactly which commands are declared and runs them automatically after the worker finishes. Workers should not rerun them merely to complete workflow verification. `verificationRepairLimit` controls automatic task repair and is independent of review rounds. Passing output is silent; failed output is bounded and routed to repair.

Each command uses `verification_timeout_ms` from `flowdex.toml`, defaulting to five minutes. Increase that setting for repositories with longer builds. Direct `flowdex.verify()` calls may override it with `timeoutMs`.

## Configure review

Use task review only when one task needs isolated scrutiny. Put `review` on a phase to batch-review the integrated changes from all tasks in that phase; attribution routes each finding to the responsible task agent. This is usually faster and cheaper than reviewing every task separately.

Reviewer instructions should identify concrete risk areas and required invariants, not restate the whole implementation plan. A review has `{ agent, instructions, maxRounds }`. The reviewer submits exactly one `report_flowdex_review` report per round; an empty findings array passes. Keep `maxRounds` small and separate from `verificationRepairLimit`. The same reviewer thread is reused across rounds.

## Program generic rounds

Use ordinary `for` or `while` loops with an explicit numeric budget. Roles are not special: two researchers, planners, or critics can exchange queued messages and advance with `resumeAgent`. Inspect terminal status each round and stop early when the workflow's condition is met. Use `keep` for short exchanges, `compact` for a long reused thread, and `handoff` when a fresh thread should receive a structured handoff.

## Save and operate

Outside workflow JavaScript, use `save_flowdex_workflow`, `start_flowdex_workflow`, and `wait_flowdex_workflow`. Handle `signal`, `message`, `steered`, `paused`, and boundary results, then wait on the same run again. Use `pause_flowdex_workflow` for a cooperative stable checkpoint and `resume_flowdex_workflow` to continue the same durable graph after pause, interruption, or failure. Integrated tasks, task worktrees, context packs, review state, signals, and boundaries are retained; a terminated JavaScript stack is not recreated. Use direct `queue_flowdex_task` and `seal_flowdex_phase` only when an awake orchestrator must modify a live open phase.
