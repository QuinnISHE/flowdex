# Flowdex Batch 009: Task worktrees

`createTask` makes a durable implementation task from the workflow worktree's
current commit. The returned handle has exactly four operations:

```js
const task = await flowdex.createTask({
  name: "update-parser",
  instructions: "Update the parser.",
  readScope: ["codex-rs/parser/**"],
  writeScope: ["codex-rs/parser/**"],
  verification: ["cargo test -p codex-parser"],
});

const result = await task.runAgent({
  name: "parser-implementer",
  instructions: "Implement the task and finish with a brief summary.",
  profile: "implementation_worker",
});
const checked = await task.verify();
const integrated = await task.integrate();
```

`runAgent` returns the ordinary bounded agent result:

```js
{ agentId, status: "completed", message? }
{ agentId, status: "errored", message? }
{ agentId, status: "shutdown" }
{ agentId, status: "notFound" }
```

After a successful task turn, Flowdex commits modifications from the isolated
worktree and attributes that commit to the agent operation. An unchanged task
needs no commit. Scopes are advisory declarations, not access controls. Roles and
repair loops remain defined by workflow instructions. A task-associated agent
can be continued through the existing `flowdex.resumeAgent` operation, which
also preserves task serialization and attribution:

```js
if (result.status !== "completed") {
  await flowdex.resumeAgent(result.agentId,
    "Repair the task, then finish with a brief summary.",
    { contextMode: "keep" });
}
```

Verification runs the stored commands in the task worktree. A passing result
binds to the exact task `HEAD`; any later commit makes that verification stale.
If no commands were configured, `verify()` rejects, but `integrate()` may still
proceed. Integration requires a clean, linear task history and atomically
cherry-picks all task commits. A conflict rolls back the whole sequence and
preserves the task for orchestrator judgment. Only one turn owns a task
worktree at a time, while distinct tasks may run concurrently.

On success, `integrate()` returns only:

```js
{
  taskId,
  commits: [{ sourceCommit, integratedCommit, agentId, model, summary }],
}
```

The source/integrated mapping carries the agent, resolved model, and commit
summary attribution. Handoff replacements returned by `resumeAgent` retain
their returned `agentId` as authoritative.

Flowdex keeps durable per-repository SQLite evidence and runtime-owned detached
task worktrees under the Codex home area. Those storage paths are implementation
details and are not exposed as JavaScript API fields.
