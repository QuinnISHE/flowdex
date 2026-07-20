---
name: run-flowdex-workflows
description: Author, save, start, wait for, and compose Flowdex JavaScript workflows. Use when a task should run through Flowdex phases, parallel agents, dependencies, verification, review, boundaries, nested workflows, signals, or direct agent-to-agent rounds without keeping the orchestrator model awake.
---

# Run Flowdex Workflows

Use a saved V8 JavaScript workflow for orchestration. Node is not required.

## Choose the execution shape

- Use `flowdex.startRun` for implementation work: durable tasks, isolated worktrees, dependencies, verification, review, integration, and boundaries.
- Use `spawnAgent`, `sendMessage`, `waitAgent`, and `resumeAgent` for small role-neutral conversations. Express rounds with ordinary `for` or `while` loops.
- Use `flowdex.runWorkflow("repo:name", input)` or `flowdex.runWorkflow("global:name", input)` for reusable nested behavior.

Check `.flowdex/workflows` before authoring. Reuse `repo:defaults/worker-reviewer` for one implementation task with review, or `repo:defaults/research-rounds` for a bounded two-agent research exchange.

## Author a run

Declare strict input, agents, phases, and tasks. Agents finish with a brief summary; Flowdex commits successful task changes and records attribution.

```js
const input = flowdex.requireInput({
  properties: {
    request: { type: "string" },
    verification: { type: "array", items: { type: "string" } },
  },
  required: ["request", "verification"],
});

const run = await flowdex.startRun({
  name: "implementation",
  agents: {
    worker: { profile: "implementation_worker" },
    reviewer: { model: "gpt-5.6-luna", reasoningEffort: "xhigh" },
  },
  phases: [{
    name: "build",
    instructions: "Implement the request and preserve existing behavior.",
    tasks: [{
      name: "change",
      agent: "worker",
      instructions: input.request,
      verification: input.verification,
      verificationRepairLimit: 1,
      review: {
        agent: "reviewer",
        instructions: "Report only concrete defects introduced by this task.",
        maxRounds: 2,
      },
    }],
  }],
});

flowdex.output(await run.wait());
```

Dependency-ready tasks run concurrently. Phases run in order. `readScope` and `writeScope` are scheduling hints, not access controls. If `open: true`, add work with `run.queueTask(...)`, then call `run.sealPhase(...)`.

## Save and dispatch

Use the direct model tools outside JavaScript:

```text
save_flowdex_workflow({ workflow: "repo:implementation", source: "<source>" })
start_flowdex_workflow({ path: "repo:implementation", input: { ... } })
wait_flowdex_workflow({ run_id: "<runId>" })
```

Wait event-first; never poll. On `steered`, `message`, or `signal`, handle the event and wait on the same run again. On an authorized boundary, call `continue_flowdex_workflow({ run_id })`, then wait again. Do not invent a progress tool: scheduler summaries and child lifecycle are automatic, app-visible, and excluded from model history.

## Program generic rounds

```js
const a = await flowdex.spawnAgent({ name: "a", instructions: "Research approach A.", profile: "explorer" });
const b = await flowdex.spawnAgent({ name: "b", instructions: "Research approach B.", profile: "explorer" });

let aResult = await flowdex.waitAgent(a);
let bResult = await flowdex.waitAgent(b);

for (let round = 0; round < 2; round++) {
  await flowdex.sendMessage(a, JSON.stringify(bResult), { delivery: "turn" });
  aResult = await flowdex.waitAgent(a);
  await flowdex.sendMessage(b, JSON.stringify(aResult), { delivery: "turn" });
  bResult = await flowdex.waitAgent(b);
}

flowdex.output({ a: aResult, b: bResult });
```

Use `resumeAgent(agentId, instructions, { contextMode: "keep" | "compact" | "handoff" })` when the caller must own the exact next turn and its terminal result. Keep verification and review round budgets separate.
