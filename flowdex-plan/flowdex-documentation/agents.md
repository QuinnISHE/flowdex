# Flowdex Batch 002: Agent workflows

The frozen `flowdex` bootstrap exposes three agent operations to saved JavaScript workflows when the existing collaboration tools are enabled. These are hidden nested tools: they are available inside a running workflow, but are not direct model tools (and `start_flowdex_workflow` remains unavailable from the workflow).

## API

`await flowdex.spawnAgent(spec)` creates a child and resolves to its thread ID as a string.

```js
flowdex.spawnAgent({
  name: "parser",
  instructions: "Implement the parser change.",
  profile: "implementation_worker", // optional
  model: "gpt-5.6-luna",             // optional
  reasoningEffort: "high",           // optional
})
```

`name` and `instructions` are required and must be non-empty after trimming. At least one selector—`profile`, `model`, or `reasoningEffort`—is required. A profile is resolved through the normal `.codex/agents` mechanism; model and reasoning effort are ordinary overrides. Spawning obeys the existing agent depth limit. No Flowdex role enum or other selector is added.

`await flowdex.sendMessage(agentId, message, options?)` resolves to `{ submissionId: string }`. `agentId` is the child thread ID and `message` must be non-empty. `options.delivery` is either `"queue"` (the default), which queues the message without starting a turn, or `"turn"`, which starts or continues the recipient's turn. Unknown delivery values are rejected.

`await flowdex.waitAgent(agentId)` subscribes to status changes and resolves when the agent reaches a terminal state; it does not poll and has no timeout argument. The result is one of:

```js
{ agentId, status: "completed", message? }
{ agentId, status: "errored",   message? }
{ agentId, status: "shutdown" }
{ agentId, status: "notFound" }
```

Completed messages (when present) and errors are bounded before being returned; the `message` field is omitted when no message is available. An interrupted/non-terminal agent does not resolve the wait until it later reaches a terminal status. Completion is status-only for Flowdex children, so it is delivered through `waitAgent` rather than injected into the parent model context.

## Saved-workflow example

```js
const agentId = await flowdex.spawnAgent({
  name: "summarize",
  instructions: `Summarize this input:\n${JSON.stringify(flowdex.input)}`,
  model: "gpt-5.6-luna",
});

await flowdex.sendMessage(agentId, "Begin now.");
const result = await flowdex.waitAgent(agentId);
emit(JSON.stringify(result));
```

## Current limits

Batch 002 provides no task or phase layer, automatic review loop, worktree assignment, tool profiles, context chunks or context-reuse modes, model-facing suspension/steering, SQLite state, or scheduler. It also does not provide Flowdex-managed verification, persistence, commits, or dynamic queue APIs.
