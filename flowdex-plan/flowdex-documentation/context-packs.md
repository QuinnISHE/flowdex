# Context packs

`contextPacks` declares reusable source context for an executable workflow.
Each pack names an existing run agent and gives that agent collection
instructions. Tasks opt in with `context`; queued tasks use the same strict
validation as tasks in the initial definition.

```js
const run = await flowdex.startRun({
  name: "update-contact-layout",
  agents: {
    explorer: { profile: "explorer" },
    implementer: { profile: "implementation_worker" },
  },
  contextPacks: {
    "contact-layout": {
      agent: "explorer",
      instructions: "Collect the current layout and invariants.",
      lifetime: "repository",
    },
  },
  phases: [{
    name: "implementation",
    instructions: "Preserve the binary layout.",
    tasks: [{
      name: "update-reader",
      agent: "implementer",
      instructions: "Update the reader.",
      context: ["contact-layout"],
    }],
  }],
});
```

Pack `lifetime` is `workflow` by default. `temporary` retains fragments through failure, pause, and recovery, then removes them only after successful workflow cleanup. `repository` loads and updates `.flowdex/context-packs/<pack>.json`, so later workflows can reuse fresh code context without another collector. Repository pack updates are generated in the publishing task worktree and included in Flowdex's attributed task commit.

Workflow and temporary packs may be seeded directly when the planner already knows bounded source ranges:

```js
plan: {
  agent: "explorer",
  instructions: "Refresh these plan requirements only if they become stale.",
  lifetime: "temporary",
  fragments: [{
    key: "requirements",
    path: "PLAN.md",
    lineStart: 10,
    lineEnd: 35,
    summary: "Requirements for this run",
  }],
}
```

Fresh seeds avoid an explorer turn. Repository packs are seeded by their checked-in file or collector so persistence remains part of a task commit.

A Flowdex child publishes a repository-relative inclusive line range with:

```text
publish_flowdex_context({
  pack,
  key,
  path,
  line_start,
  line_end,
  summary?,
}) -> { pack, key, version }
```

Publishing the same pack and key creates a new immutable version and
supersedes the previous active version. Flowdex stores the selected text and
its hash outside conversation history. Paths are resolved against the child's
current worktree and may not escape it through absolute paths, parent
components, links, reparse points, directories, or invalid ranges.

Before a dependent task starts, Flowdex re-reads each active range from the
current integration worktree. Fresh fragments are ordered by key and appended
once to the task instructions. Missing or changed ranges make the pack missing
or stale and block only dependent tasks. Distinct packs needed by the same or different ready tasks collect concurrently. The scheduler starts one ordinary
`StatusOnly` collector per run and pack, using the declared agent and normal
capacity, profile, cancellation, and lifecycle behavior. Other ready tasks
continue. A collector that fails or finishes without a fresh pack fails the
affected scheduler path instead of looping.

Workers receiving a repository pack are told to republish a fragment only when their changes invalidate its meaning. Incidental edits that leave the context accurate do not require an update.

Models can inspect a pack directly when needed:

```text
read_flowdex_context({ pack }) -> {
  pack,
  status: "fresh" | "missing" | "stale",
  fragments: [{
    key,
    version,
    path,
    lineStart,
    lineEnd,
    summary?,
    content,
  }],
}
```

Normal task injection reads persistence internally; workflow JavaScript and
the orchestrator do not load fragment content. Scheduler progress such as
`Collecting context: contact-layout` and `Context ready: contact-layout` is
live, non-persistent UI state and does not enter model history.
