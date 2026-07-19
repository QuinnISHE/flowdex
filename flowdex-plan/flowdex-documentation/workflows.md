# Executable workflows

`startRun` starts one durable scheduler run for the saved workflow cell. It
resolves to a frozen handle with exactly `id`, `queueTask`, `sealPhase`, and
`wait`:

```js
const run = await flowdex.startRun({
  name: "update-record-layout",
  boundary: "continue",
  agents: {
    implementer: { profile: "implementation_worker", toolProfile: "development" },
    fastFix: { model: "gpt-5.6-luna", reasoningEffort: "high" },
  },
  phases: [
    {
      name: "implementation",
      instructions: "Implement the record-layout changes and commit each task.",
      boundary: "continue",
      review: { agent: "fastFix", instructions: "Review the integrated phase changes.", maxRounds: 2 },
      verification: ["cargo test -p record-layout"],
      tasks: [
        {
          name: "parser",
          agent: "implementer",
          instructions: "Update the parser.",
          readScope: ["src/parser/**"],
          writeScope: ["src/parser/**"],
          verification: ["cargo test -p parser"],
          verificationRepairLimit: 2,
          review: { agent: "fastFix", instructions: "Review this task's committed diff.", maxRounds: 2 },
          boundary: "continue",
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
// or { runId, status: "boundary", scope: { kind, name }, target, reason }
```

Phases run in declaration order. Ready tasks run concurrently within the
existing shared agent capacity; dependencies determine semantic ordering. The
scheduler integrates completed tasks in declaration order, and avoids an
obvious equal-or-ancestor write-scope collision by serializing that pair.
Scopes remain advisory declarations, not access controls. The phase and run
verification commands execute separately against the integrated worktree.
Declared agents use the same resolved tool profile for initial execution,
verification repair, task and phase review, attributed review repair,
context-pack collection, and handoff replacements. Resolution starts from the
parent configuration snapshot, then applies the `.codex/agents` profile, the
Flowdex tool profile, and finally explicit model and reasoning overrides.

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
error and preserves task/worktree evidence.

## Review, repair, and boundaries

`boundary` is exactly `"continue"`, `"orchestrator"`, or `"human"`; omission
means `"continue"`. `verificationRepairLimit` is a non-negative integer and
defaults to zero. An optional `review` has exactly `agent`, `instructions`, and
positive integer `maxRounds`; the agent must exist in `agents` and has no
special role. Verification and review budgets are independent. Exhaustion or
an unattributed finding pauses at the orchestrator (or human for a `"human"`
boundary); unresolved findings never auto-continue.

During a review, the declared agent can call only:

```js
report_flowdex_review({ findings: [{
  file: "src/parser.rs", lineStart: 42, lineEnd: 48,
  reason: "Field order is incorrect.", ruleKey: "parser.field-order",
  astGrepSuitable: false,
}] });
```

Findings require non-empty `file` and `reason`, positive inclusive lines with
`lineEnd >= lineStart`, optional non-empty `ruleKey`, and required boolean
`astGrepSuitable`; unknown fields are rejected. Exactly one accepted report is
required per review operation. `findings: []` is an explicit pass; completion
without a report is an error. Attribution follows finding lines to the
integrated commit, source commit, task operation, and agent, then falls back to
the most recent run commit touching the file. Unattributed findings pause.
Repairs are sent directly to the attributed task agent, reverifed, and consume
a review round only when the next report still has findings.

Minimal non-review saved workflow (ordinary JavaScript and numeric budget):

```js
const a = await flowdex.spawnAgent({ name: "research-a", instructions: "Research the question.", model: "gpt-5.6-luna" });
const b = await flowdex.spawnAgent({ name: "research-b", instructions: "Research the question independently.", model: "gpt-5.6-luna" });
for (let round = 0; round < 2; round++) {
  const result = await flowdex.resumeAgent(a, `Round ${round}: report findings.`);
  await flowdex.sendMessage(b, JSON.stringify(result), { delivery: "turn" });
  const reply = await flowdex.waitAgent(b);
  if (reply.status !== "completed") break;
}
```

Scheduler transitions emit concise automatic reasoning summaries through the
live app UI. They are not callable progress methods, are not added to model
context or persisted conversation history, and are absent from workflow
results. Task agents still use normal app-visible lifecycle and status events;
roles and any loops remain defined by workflow instructions. A run is durable
for execution and inspection, but process-restart resumption and orphan cleanup
are not supported.
