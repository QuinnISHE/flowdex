---
name: collect-flowdex-context
description: Declare, collect, publish, refresh, and inspect Flowdex context packs. Use when explorer agents should supply exact repository context directly to dependent workflow tasks without routing file contents through the orchestrator model.
---

# Collect Flowdex Context

Use context packs for source facts that tasks need but the orchestrator does not.

## Declare the pack

```js
const run = await flowdex.startRun({
  name: "parser-change",
  agents: {
    explorer: { profile: "explorer" },
    worker: { profile: "implementation_worker" },
  },
  contextPacks: {
    parser: {
      agent: "explorer",
      instructions: "Publish the parser format, invariants, and relevant tests as exact source ranges.",
    },
  },
  phases: [{
    name: "build",
    instructions: "Preserve the documented format.",
    tasks: [{
      name: "update-parser",
      agent: "worker",
      instructions: "Implement the parser change.",
      context: ["parser"],
    }],
  }],
});
```

Only tasks listing the pack in `context` receive its fragments.

## Publish exact fragments

As the collector, publish repository-relative inclusive line ranges:

```text
publish_flowdex_context({
  pack: "parser",
  key: "wire-format",
  path: "src/parser.rs",
  line_start: 40,
  line_end: 78,
  summary: "Record header and field ordering"
})
```

Use stable, descriptive keys. Prefer the smallest complete range that preserves the invariant. Publish separate fragments for separate facts. Never paste fragment contents into a parent message.

Publishing the same `pack` and `key` creates a new version that supersedes the old one. Flowdex re-reads active ranges from the integration worktree before dispatch. If a range changed, the pack becomes stale and the scheduler starts one collector to refresh it; unrelated ready tasks continue.

## Inspect only when needed

```text
read_flowdex_context({ pack: "parser" })
```

Use the read tool to diagnose missing or stale context. Normal dependent-task injection is automatic and does not load fragment contents into workflow JavaScript or orchestrator history.
