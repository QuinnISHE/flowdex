# Executable workflows

`startRun` starts one durable scheduler run for the saved workflow cell. It
resolves to a frozen handle with exactly `id`, `queueTask`, `sealPhase`, and
`wait`:

```js
const run = await flowdex.startRun({
  name: "update-record-layout",
  agents: {
    implementer: { profile: "implementation_worker" },
    fastFix: { model: "gpt-5.6-luna", reasoningEffort: "high" },
  },
  phases: [
    {
      name: "implementation",
      instructions: "Implement the record-layout changes and commit each task.",
      verification: ["cargo test -p record-layout"],
      tasks: [
        {
          name: "parser",
          agent: "implementer",
          instructions: "Update the parser.",
          readScope: ["src/parser/**"],
          writeScope: ["src/parser/**"],
          verification: ["cargo test -p parser"],
        },
        {
          name: "serializer",
          agent: "implementer",
          instructions: "Update the serializer.",
          readScope: ["src/serializer/**"],
          writeScope: ["src/serializer/**"],
        },
        {
          name: "round-trip",
          agent: "fastFix",
          instructions: "Add round-trip coverage after both changes integrate.",
          dependencies: ["parser", "serializer"],
          readScope: ["src/**"],
          writeScope: ["tests/**"],
        },
      ],
    },
  ],
});

const result = await run.wait();
// { runId, status: "completed" }
```

Phases run in declaration order. Ready tasks run concurrently within the
existing shared agent capacity; dependencies determine semantic ordering. The
scheduler integrates completed tasks in declaration order, and avoids an
obvious equal-or-ancestor write-scope collision by serializing that pair.
Scopes remain advisory declarations, not access controls. The phase and run
verification commands execute separately against the integrated worktree.

An open phase can receive tasks while it is active. A saved workflow uses the
handle methods; an awake orchestrator may use the direct model-only
`queue_flowdex_task({ run_id, phase, task })` and
`seal_flowdex_phase({ run_id, phase })` tools:

```js
const run = await flowdex.startRun({
  name: "triage",
  agents: { implementer: { profile: "implementation_worker" } },
  phases: [{
    name: "work",
    instructions: "Resolve the reported issue and commit the change.",
    open: true,
    tasks: [],
  }],
});

await run.queueTask("work", {
  name: "diagnose",
  agent: "implementer",
  instructions: "Diagnose and fix the issue.",
  readScope: ["src/**"],
  writeScope: ["src/**"],
});
await run.sealPhase("work");
await run.wait();
```

`queueTask` returns `{ taskId }`; `sealPhase` resolves to `undefined`. Dynamic
tasks may depend only on tasks already present, and a rejected addition leaves
the running workflow unchanged. Definitions and queued tasks reject null,
arrays, primitives, unknown keys, blank strings, unknown agents, duplicate
names, missing dependencies or cycles, and invalid command arrays. The initial
definition is validated atomically.

The first task, phase, or run failure (including agent, verification, commit
attribution, or integration failure) rejects `wait()` with the bounded tool
error and preserves task/worktree evidence. There are no repair loops, review
budgets, boundaries, signals, generic waits, profiles, configurable Flowdex
concurrency controls, or scheduler restart recovery in this version.

Scheduler transitions emit concise automatic reasoning summaries through the
live app UI. They are not callable progress methods, are not added to model
context or persisted conversation history, and are absent from workflow
results. Task agents still use normal app-visible lifecycle and status events;
roles and any loops remain defined by workflow instructions. A run is durable
for execution and inspection, but process-restart resumption and orphan cleanup
are not supported.
