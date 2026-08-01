# Flowdex Batch 023: Context lifetimes and focused agents

## Goal

Extend the existing context-pack and scheduler lifecycles without creating a second context system. Add repository-lived packs, explicitly temporary packs, planner-provided source fragments, successful-run cleanup, parallel pack preparation, and configurable Flowdex child exclusions.

## Workflow contract

Each `contextPacks` entry keeps required `agent` and `instructions` and accepts:

```js
contextPacks: {
  architecture: {
    agent: "explorer",
    instructions: "Collect the parser invariants used by implementation tasks.",
    lifetime: "repository", // workflow (default), temporary, or repository
  },
  plan: {
    agent: "explorer",
    instructions: "Refresh only if the supplied plan ranges become stale.",
    lifetime: "temporary",
    fragments: [{
      key: "requirements",
      path: "flowdex-plan/PLAN.md",
      lineStart: 20,
      lineEnd: 60,
      summary: "Requirements used by this run",
    }],
  },
},
cleanup: ["remove the untracked planner scratch file"],
```

- `workflow` preserves the current run-scoped durable behavior.
- `temporary` is equally durable during execution, failure, pause, and recovery, then its stored fragments are deleted only after a successful terminal cleanup.
- `repository` loads and writes `.flowdex/context-packs/<pack>.json`. Repository pack names must be safe single path components. Git history owns long-term versioning; the file contains the current active fragments.
- `fragments` seeds source-backed ranges before scheduling and is allowed for workflow and temporary packs. Repository packs are seeded through their checked-in file or their collector so persistence remains part of an attributed task commit.
- `cleanup` is an ordered list of commands run in the integration worktree only after final verification and boundary continuation. Failure fails the run and preserves temporary context for recovery. Commands must be non-interactive, idempotent, and leave the integration worktree usable.

## Context behavior

Hydrate repository files and declared seed fragments into the existing SQLite fragment/version model before agents run. Continue to compare source ranges for freshness. Publishing a repository fragment updates its checked-in pack file in the active task worktree; Flowdex's existing successful-operation commit and integration lifecycle carries that update into the repository.

When a task receives repository context, add one short instruction: republish only fragments whose meaning was invalidated by the task's edits. Do not require publication for incidental line changes that leave the context accurate.

Resolve all distinct packs requested by a ready task concurrently. Keep the existing per-run/per-pack gate so two tasks requesting the same pack share one collector while different packs collect in parallel.

## Focused Flowdex children

Add strict global/repository Flowdex settings:

```toml
subagent_excluded_tools = []
subagent_excluded_skills = ["run-flowdex-workflows"]

[tool_profiles.explorer]
excluded_tools = ["apply_patch"]
excluded_skills = []
```

Global values load first and a present trusted-repository list replaces the corresponding global list. Tool-profile lists extend the defaults for the selected Flowdex child only. Filter exact tool names before tool-search/code-mode composition and disable matching skill names through the child session's existing skill configuration layer. Ordinary Codex subagents and the root orchestrator remain unchanged.

Update the workflow skill to recommend parallel independent packs, repository packs for stable code facts, temporary seeded packs for run-only plan context, and role-specific tool profiles for explorers and reviewers.

## Focused evidence

Use narrow tests for strict workflow parsing/lifetime validation, repository round-trip and stale detection, temporary successful cleanup versus failed retention, concurrent distinct-pack collection, and child exclusion resolution. Run formatting and targeted crate checks only after the implementation is complete; do not run a full workspace build.
