# Flowdex Plan

## Summary

Flowdex adds model-authored JavaScript workflows to the Codex backend. The planner saves a workflow under `.flowdex/workflows/` and starts it through a model tool. The runtime then manages agents, task queues, verification, reviews, context, and event-driven suspension without repeatedly waking the orchestrator.

Plain JavaScript runs through Codex's existing native V8 code-mode runtime. Rust owns durable orchestration, Git worktrees, events, attribution, and SQLite state.

## Workflow and Agent Interfaces

- A workflow contains sequential phases. Each phase owns an open task queue containing dependency-aware tasks.
- Tasks can be queued when the workflow is created or dynamically while their phase is active and unsealed.
- Phases declare:
  - Instructions inherited by every task in the phase.
  - Initial tasks and dependency relationships.
  - Phase-level verification.
  - Optional review behavior.
  - Boundary behavior: continue automatically, wake the orchestrator, or pause for the human.
- Tasks declare:
  - Name, dependencies, and instructions.
  - Advisory read/write scopes.
  - Verification commands and a verification repair limit.
  - Optional review configuration with a separate review round limit.
  - Required context packs.
  - Boundary behavior.
  - The agent configuration used to execute them.
- Workflows declare reusable agents. Each agent must specify at least one of:
  - Model.
  - Reasoning effort.
  - Existing `.codex/agents` profile.
- Agents may also reference a Flowdex tool profile. Named agent profiles provide defaults; explicit workflow settings override only the fields they specify.
- No worker, reviewer, or explorer role is encoded in the runtime. Any agent may implement, inspect, gather context, or review when given the corresponding instructions and tools.

### Generic primitives

- Spawn or resume an agent.
- Send a direct message to any agent in the run.
- Queue a task into an active or future unsealed phase.
- Execute configured verification and return a structured result.
- Run an agent with a selected tool profile.
- Route structured findings through commit attribution.
- Await workflow, agent, command, message, signal, or user-steering events.
  - Publish or supersede a context chunk.
- Emit a workflow progress summary.

Messages go directly to the target thread without passing through the orchestrator. A busy agent receives the message at its next safe boundary; an idle reusable agent is resumed. Delivery does not create an orchestrator turn.

## Runtime Implementation

### Workflow scheduling

- Add a dedicated Flowdex Rust crate rather than expanding `codex-core`.
- Add the JavaScript workflow package and model-facing workflow-start tool.
- Execute workflow files under the existing Codex sandbox and approval policy.
- Validate initial task dependencies before starting.
- Validate dynamically queued tasks independently. Reject an invalid addition without terminating unrelated running tasks.
- Run phases sequentially and dependency-ready tasks concurrently.
- Keep a phase open while its workflow code and agents may still queue work. Seal it before phase verification, review, and boundary handling.
- Do not reopen completed phases. Additional work discovered later is queued into an active/future phase or a newly declared phase without restarting the run.

### Agent execution and Git integration

- Give implementing agents isolated Git worktrees.
- Run task verification in the task worktree before accepting its commit.
- Commit successful changes with a detailed summary and record the run, task, agent thread, model, and commit hash.
- Integrate accepted commits into the run branch. Dependent tasks start from the integrated dependency state.
- Wake the orchestrator when integration conflicts require judgment.
- Run phase verification against the integrated phase state and run verification against the final run state.

### Verification and review composition

- Verification and review remain separate operations with separate budgets:
  - Verification failures consume the verification repair limit.
  - Review findings consume the review round limit.
- Passing verification continues silently.
- Failed verification returns bounded command output directly to the agent selected by the workflow.
- Review is implemented by running any agent with review instructions and the structured reporting tool.
- The runtime provides the primitives needed for repair/review loops but does not impose a worker-reviewer pairing or fixed loop.
- Structured findings contain file, line range, reason, optional stable rule key, and AST-grep suitability.
- Finding routing uses the last Flowdex commit touching the reported lines, then file-level attribution, then orchestrator escalation.
- Round exhaustion follows the configured boundary: orchestrator by default or human when requested.

## Context Packs

- Flowdex context chunks are immutable and versioned.
- Each chunk records:
  - Pack and chunk key.
  - Captured content.
  - Source file and line range.
  - Source commit.
  - The chunk version it supersedes, when applicable.
- Agents update context by publishing a new chunk that supersedes the previous version. Historical runs retain the exact version they consumed.
- When an integrated commit changes lines covered by a chunk, mark that chunk stale.
- Pack resolution selects the newest non-stale chunk for each key.
- Existing agent prompts remain stable; updated chunks apply to later task dispatches.
- If a required chunk is missing or stale:
  - Automatically queue a context-gathering task using the configured agent and tool profile.
  - Suspend only the dependent task while collection runs.
  - Resume the task when refreshed context is published.
  - Escalate to the orchestrator only if the bounded collection attempt fails.
- Context is injected directly into the consuming agent’s prompt and never routed through the orchestrator.
- Keep individual chunks and total injected packs within the existing bounded-context requirements. Codex's existing context-fragment/context-fragment-update protocol naming remains unchanged where referenced.

## Events, Steering, and Progress

- Implement waits with runtime channels and app-server notifications rather than timers or repeated status calls.
- Material wake events include:
  - Workflow, phase, or task completion.
  - Agent message or completion.
  - Command completion.
  - Verification or review escalation.
  - Human checkpoint.
  - Explicit workflow signal.
  - User steering.
- A user `turn/steer` event always interrupts a pending Flowdex wait and wakes the orchestrator with the new input.
- Steering does not automatically cancel the workflow. The orchestrator may inspect, modify, pause, or cancel it after receiving the user’s direction.
- Emit concise Flowdex progress through the existing reasoning-summary notification channel so the current Codex GUI displays updates such as the active phase, task progress, verification, review, or checkpoint state.
- Progress summaries are UI-only runtime notifications:
  - Do not add them to conversation history.
  - Do not persist them as model-visible rollout items.
  - Do not include them in subsequent inference requests.
- Emit progress only for meaningful state transitions and coalesce repetitive task updates.

## Human Boundaries and Context Compaction

- Task, phase, and run boundaries support:
  - Automatic continuation.
  - Orchestrator wake.
  - Human approval or revision.
- Human approval resumes the runtime directly.
- Human revision feedback wakes the orchestrator to coordinate changes.
- Add a model-visible manual context-compaction tool.
- At a configurable threshold, defaulting to 150,000 tokens, remind the model to compact at the next discrete task boundary.
- Agent reuse supports:
  - `keep`: resume the existing thread.
  - `compact`: compact and resume.
  - `handoff`: collect a structured handoff and start a fresh thread.

## Review History and AST-grep Rules

- Persist review findings and the later commits that resolve them.
- Group resolved findings by reviewer-provided stable rule key.
- Mark a key as a candidate after three resolved occurrences by default.
- Do not run background model-based semantic clustering.
- Provide a user-started candidate scan that dispatches the rule-writing agent or skill.
- Require individual human approval before writing each repository AST-grep rule.
- Configure accepted rules as either always-on verification or explicitly requested by a workflow.

## Persistence, Configuration, and Installation

- Save this plan as `Flowdex Plan.md` when implementation begins.
- Load global settings from `$CODEX_HOME/flowdex.toml`.
- Overlay repository settings from `.flowdex/config.toml`.
- Use these files for defaults, tool profiles, context-gathering agents, round limits, compaction threshold, and AST-grep behavior.
- Keep one per-repository SQLite database under `$CODEX_HOME/flowdex/`.
- Store runs, phases, tasks, dynamic queue entries, events, messages, agent/commit attribution, context-chunk versions, review history, and rule candidates.
- Add `codex flowdex install --binary <absolute-path>` on Windows:
  - Validate the compiled Flowdex executable.
  - Configure the current user’s Codex Windows app backend override.
  - Tell the user to restart the app.
- Determine the app-owned backend environment-variable name during installer implementation because it is outside this CLI repository.

## Test Plan

- Build one end-to-end workflow covering phase inheritance, parallel tasks, dynamic queuing, direct agent messages, worktrees, integration, and completion.
- Verify passing commands cause no additional model turn and failed commands return to the selected repair agent.
- Verify verification and review consume independent budgets.
- Verify structured findings route through commit attribution without a hard-coded reviewer role.
- Verify stale chunks are superseded and missing context queues a context-gathering task.
- Verify a pending event wait wakes immediately on user steering.
- Verify progress summaries reach app-server clients but never appear in the next model request.
- Verify human approval/revision and the three context-reuse modes.
- Add focused persistence, AST-grep candidate, and Windows installer coverage.

## Defaults and Boundaries

- Flowdex uses the existing native V8 code-mode runtime; Node is not required for workflow execution.
- The model starts workflows; no manual workflow-run CLI is added initially.
- The older workflow example and repair/review loop are behavioral examples, not API compatibility requirements.
- File scopes remain advisory.
- Worker, reviewer, and explorer are prompt-level conventions rather than runtime types.
- No GUI modifications, visual workflow editor, compatibility shim, feature flag, background semantic classifier, or generalized orchestration graph is added.
