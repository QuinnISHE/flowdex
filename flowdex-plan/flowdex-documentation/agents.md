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

`await flowdex.resumeAgent(agentId, instructions, { contextMode })` dispatches one new turn on an existing child and resolves with the terminal result for that newly submitted operation. The options object is optional and defaults to `{ contextMode: "keep" }`; it accepts only `contextMode`:

- `"keep"` retains the existing thread and conversation history.
- `"compact"` completes native compaction on the existing thread before the new instructions start, then retains that thread.
- `"handoff"` obtains a bounded structured handoff from the completed old thread, starts a fresh sibling with fresh history and the old thread's resolved configuration, and runs the handoff followed by the new instructions. The old thread remains intact; the returned `agentId` is the authoritative fresh sibling ID.

The result uses exactly the same shapes as `waitAgent` above. For `keep` and `compact`, `agentId` remains the original ID; for `handoff`, it is the fresh sibling ID. The promise waits for this submitted turn, not the target's prior terminal status. All three modes preserve StatusOnly completion isolation, and optional `message` values are bounded as with `waitAgent`.

`sendMessage` is the general queue/turn messaging primitive and returns a submission ID; it does not own completion. `waitAgent` observes the current thread until terminal completion. `resumeAgent` is the discrete-turn operation that both submits instructions (and any selected context transition) and owns that operation's completion.

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

This batch provides no task/phase layer or scheduler, worktree management, durable replacement alias or mapping, custom handoff prompt, or tool profiles. It also provides no automatic review loop, SQLite state, persistence, commits, dynamic queues, or context thresholds/strategies. Batch 005 adds model-facing event-driven suspension and steering through [waiting.md](waiting.md); see [verification.md](verification.md) for the Batch 003 verification primitive and [progress.md](progress.md) for transient progress summaries.

Native model-requested context compaction is documented in [compaction.md](compaction.md). It is available to direct model turns only; saved workflows cannot invoke it.
