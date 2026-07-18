# Flowdex Batch 004: Progress summaries

The frozen `flowdex` bootstrap exposes `progress` to a running saved workflow.
It publishes a concise, transient update through Codex's existing reasoning-summary
UI path.

## API

```js
await flowdex.progress("Verifying the parser changes");
```

`summary` must be a string that is non-empty after trimming. The displayed text
is trimmed and bounded by the existing 4096-token text limit. The promise
resolves to JavaScript `undefined`.

## Workflow example

```js
const agentId = await flowdex.spawnAgent({
  name: "parser",
  instructions: "Implement the parser change.",
  model: "gpt-5.6-luna",
});

await flowdex.progress("Parser work started");
const result = await flowdex.waitAgent(agentId);
await flowdex.progress("Parser work finished; checking the result");
emit(JSON.stringify(result));
```

## Transient behavior and limits

Each accepted call appears live as one completed existing reasoning item for
current UI subscribers. It is UI-only and transient: it is not persisted in
rollout or conversation history, is not included in a later model request, and
is not returned through the parent workflow result. Consequently, summaries
cannot be recovered after a workflow is resumed.

Workflow authors explicitly call `progress` at meaningful transitions, such as
starting work or beginning verification. Automatic task- or phase-generated
progress does not exist. This primitive adds no task, phase, scheduler,
deduplication, or durable progress-history API.
