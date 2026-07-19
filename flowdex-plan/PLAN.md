# Flowdex Plan

## Summary

Flowdex adds model-authored JavaScript workflows to the Codex backend. The planner can save a repository workflow under `.flowdex/workflows/` or a reusable global workflow under `$CODEX_HOME/flowdex/workflows/`, then start it through a model tool. The runtime manages agents, task queues, verification, reviews, context, nested workflows, and event-driven suspension without repeatedly waking the orchestrator.

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

### Saved and nested workflows

- Entry workflows and reusable workflows use the same saved JavaScript format. An implementation plan is one workflow shape, not a separate runtime type.
- Repository workflows live under `.flowdex/workflows/`. Global workflows live under `$CODEX_HOME/flowdex/workflows/` and are available across trusted repositories.
- References identify their scope explicitly, such as `repo:documentation/check` or `global:documentation/check`, so repository files never silently shadow global files.
- A saved workflow may declare a strict JSON-compatible input object schema using the same small object/property/required vocabulary used by Codex tool schemas. Flowdex validates input before starting the workflow and exposes only the validated value to its JavaScript.
- A workflow may conditionally invoke another saved workflow, pass validated input, await it without a model turn, and receive a JSON-compatible result.
- Nested runs retain their workflow identity and parent run relationship. They reuse normal scheduler state, automatic progress, app-visible agent events, cancellation, and event-driven waiting.
- Parent cancellation propagates to active nested runs. User steering still wakes the orchestrator without implicitly cancelling either run.
- Reject an active-chain workflow cycle rather than allowing accidental recursive execution. Do not add a second JavaScript runtime or a generic import/package system.
- Provide one scoped model authoring operation for saving or updating named workflows. It may write only beneath the selected repository or global workflow root; repository workflow authoring and execution remain trust-gated.

### JavaScript authoring model

The language stays ordinary asynchronous JavaScript with a small Flowdex API. A saved file declares its input contract and workflow body; the body may construct one implementation run, invoke reusable child workflows, or do both conditionally.

The intended durable vocabulary is:

- **Workflow definition:** name, strict input schema, and asynchronous body.
- **Run definition:** reusable agents, run verification, and ordered phase definitions.
- **Phase definition:** inherited instructions, tasks, verification, review, boundary behavior, and whether its dynamic queue is open.
- **Task definition:** name, selected agent, instructions, dependencies, advisory scopes, verification, review, context requirements, and boundary behavior.
- **Nested workflow invocation:** scoped workflow name plus input, returning a structured result.
- **Dynamic control:** queue a task into an open phase, seal the phase, and await the run.
- **Agent control:** spawn or resume an agent and send direct agent-to-agent messages when the high-level task scheduler is not the right abstraction.
- **Verification and review:** execute structured verification and route structured findings without hard-coding worker or reviewer roles.
- **Context:** request a context pack and publish or supersede a fragment.
- **Suspension:** await scheduler state, agent completion, signals, messages, or steering without polling.

Common implementation workflows should mostly use run, phase, and task declarations. The lower-level agent and verification primitives remain available for generic workflows that do not fit the implementation-task lifecycle. Automatic progress is runtime-owned and intentionally absent from this vocabulary.

### Generic primitives

- Spawn or resume an agent.
- Send a direct message to any agent in the run.
- Queue a task into an active or future unsealed phase.
- Execute configured verification and return a structured result.
- Run an agent with a selected tool profile.
- Route structured findings through commit attribution.
- Await workflow, agent, command, message, signal, or user-steering events.
- Invoke a saved repository or global workflow with validated input and receive its structured result.
- Publish or supersede a context fragment.

Workflow progress is automatic runtime behavior attached to scheduler transitions. It is not a model-callable or workflow-callable primitive.

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

- Context fragments are immutable and versioned.
- Each fragment records:
  - Pack and fragment key.
  - Captured content.
  - Source file and line range.
  - Source commit.
  - The fragment version it supersedes, when applicable.
- Agents update context by publishing a new fragment that supersedes the previous version. Historical runs retain the exact version they consumed.
- When an integrated commit changes lines covered by a fragment, mark that fragment stale.
- Pack resolution selects the newest non-stale fragment for each key.
- Existing agent prompts remain stable; updated fragments apply to later task dispatches.
- If a required fragment is missing or stale:
  - Automatically queue a context-gathering task using the configured agent and tool profile.
  - Suspend only the dependent task while collection runs.
  - Resume the task when refreshed context is published.
  - Escalate to the orchestrator only if the bounded collection attempt fails.
- Context is injected directly into the consuming agent’s prompt and never routed through the orchestrator.
- Keep individual fragments and total injected packs within the existing bounded-context requirements.

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
- Use these files for tool profiles, compaction threshold, AST-grep behavior, and other defaults only when a completed feature consumes them. Keep agent selection and round budgets explicit in workflow definitions.
- Keep one per-repository SQLite database under `$CODEX_HOME/flowdex/`.
- Store runs, phases, tasks, dynamic queue entries, events, messages, agent/commit attribution, context-fragment versions, and review history. Derive rule candidates from that durable history instead of duplicating candidate state.
- Ship the compiled CLI as `flowdex.exe` on Windows or `flowdex` on macOS; `flowdex install` copies itself into `$CODEX_HOME/flowdex/bin` and selects it as the desktop backend, while `flowdex uninstall` removes both.
  - Validate the compiled Flowdex executable.
  - Configure the current user’s Windows or macOS Codex app backend override without modifying the app bundle.
  - Tell the user to restart the app.

## Test Plan

- Build one end-to-end workflow covering phase inheritance, parallel tasks, dynamic queuing, direct agent messages, worktrees, integration, and completion.
- Verify passing commands cause no additional model turn and failed commands return to the selected repair agent.
- Verify verification and review consume independent budgets.
- Verify structured findings route through commit attribution without a hard-coded reviewer role.
- Verify stale fragments are superseded and missing context queues a context-gathering task.
- Verify a pending event wait wakes immediately on user steering.
- Verify progress summaries reach app-server clients but never appear in the next model request.
- Verify human approval/revision and the three context-reuse modes.
- Add focused persistence, AST-grep candidate, and Windows installer coverage.

## Defaults and Boundaries

- Flowdex JavaScript runs in Codex's existing native V8 code-mode runtime; Node is not a prerequisite.
- The model starts workflows; no manual workflow-run CLI is added initially.
- The older workflow example and repair/review loop are behavioral examples, not API compatibility requirements.
- File scopes remain advisory.
- Worker, reviewer, and explorer are prompt-level conventions rather than runtime types.
- No GUI modifications, visual workflow editor, compatibility shim, feature flag, background semantic classifier, or generalized orchestration graph is added.
