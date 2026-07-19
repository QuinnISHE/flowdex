# Flowdex getting started

Flowdex workflows are ordinary asynchronous JavaScript executed by Codex's native V8 runtime (Node is not required).

## Build and install

From the repository root, build the modified CLI:

```text
cargo build -p codex-cli --bin codex
```

Install that executable as the desktop backend on Windows or macOS:

```text
codex flowdex install --binary C:\absolute\path\to\codex.exe
codex flowdex install --binary /absolute/path/to/codex
```

The path must be absolute and point to a working Codex executable. Restart the desktop app after installation. See [the installer contract](windows-app-installer.md).

## Save a workflow

Repository workflows are saved under `.flowdex/workflows/` in a trusted repository. Global reusable workflows are under `$CODEX_HOME/flowdex/workflows/` and are available across trusted repositories. The model saves source with `save_flowdex_workflow`, naming the target explicitly, for example `repo:checks/docs` or `global:checks/docs`; it then starts it with `start_flowdex_workflow({ path, input })`.

Here is a complete small workflow module. It demonstrates strict input, reusable agents, phases, dependency-ready tasks, a required context pack, verification and review, dynamic queueing/sealing, and a human boundary:

```js
const input = flowdex.requireInput({
  properties: { files: { type: "array", items: { type: "string" } } },
  required: ["files"],
});

const run = await flowdex.startRun({
  name: "document-check",
  boundary: "human",
  agents: {
    editor: { model: "gpt-5.6-luna", reasoningEffort: "high" },
    reviewer: { profile: "implementation_worker_fast" },
  },
  contextPacks: {
    conventions: { agent: "editor", instructions: "Collect repository documentation conventions." },
  },
  phases: [{
    name: "inspect",
    instructions: "Inspect the requested files and commit changes.",
    open: true,
    tasks: [
      { name: "scan", agent: "editor", instructions: `Check ${input.files.join(", ")}.`, context: ["conventions"], verification: ["git diff --check"] },
      { name: "independent", agent: "editor", instructions: "Check links independently." },
      { name: "final", agent: "reviewer", dependencies: ["scan", "independent"], instructions: "Review the integrated result.", review: { agent: "reviewer", instructions: "Report concrete defects.", maxRounds: 1 }, boundary: "orchestrator" },
    ],
  }],
});

await run.queueTask("inspect", { name: "follow-up", agent: "editor", dependencies: ["final"], instructions: "Apply any final corrections." });
await run.sealPhase("inspect");
const result = await run.wait();
flowdex.output(result);
```

An open phase accepts dynamic tasks until `sealPhase`; dependency-ready tasks run concurrently when capacity permits. Scopes are advisory declarations. Verification and review budgets are independent, and `boundary` may be `continue`, `orchestrator`, or `human`.

The model starts and waits for the saved cell:

```text
start_flowdex_workflow({ path: "repo:checks/docs", input: { files: ["README.md"] } })
wait_flowdex_workflow({ run_id: "<runId from start>" })
```

`wait_flowdex_workflow` is event-driven: it wakes for completion, boundaries, messages, signals, or steering; it does not poll. Continue a boundary with `continue_flowdex_workflow({ run_id })`. Scheduler progress and child-agent lifecycle remain visible in the app, but are not added to model history or workflow results.

## Composition and direct messaging

Inside a saved workflow, call a named child with `await flowdex.runWorkflow("global:checks/docs", input)`; the child validates its own `requireInput` schema and returns its exact JSON output. Do not call the direct-model `start_flowdex_workflow` tool from JavaScript.

For a small role-neutral conversation, ordinary JavaScript is enough:

```js
const a = await flowdex.spawnAgent({ name: "a", instructions: "Investigate one angle.", model: "gpt-5.6-luna" });
const b = await flowdex.spawnAgent({ name: "b", instructions: "Investigate another angle.", model: "gpt-5.6-luna" });
const note = await flowdex.resumeAgent(a, "Share your findings.");
await flowdex.sendMessage(b, JSON.stringify(note), { delivery: "turn" });
await flowdex.waitAgent(b);
```

For detailed contracts, see [executable workflows](workflows.md), [reusable workflows](reusable-workflows.md), [agents](agents.md), [context packs](context-packs.md), [verification](verification.md), [review findings](reviews.md), and [event-driven waits](waiting.md).
