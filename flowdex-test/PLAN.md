# Flowdex validation plan

This fixture exercises the installed Flowdex backend without building Codex or modifying product source files.

## Run

1. Confirm Codex was fully restarted after installing the newly compiled binary. Do not build, test, or edit Codex source.
2. Record `git status --short` and the current HEAD so the final report can distinguish pre-existing changes from workflow output.
3. Start the saved workflow:

   ```text
   start_flowdex_workflow({
     path: "repo:flowdex-test/full-validation",
     input: { label: "manual" }
   })
   ```

4. If it yields, use `wait_flowdex_workflow({ run_id })` once and wait event-first. Never poll or list agents repeatedly.
5. A `signal` result named `preflight_complete` is expected. Record it, then wait on the same run again.
6. If steering or a trigger message wakes the wait, handle it normally and wait on the same run again. Stop on `failed` or `terminated` and report the bounded error unchanged.
7. Continue until the workflow is terminal. Do not edit a result file to make the test pass.

## Expected behavior

- The app shows both nested messaging agents, task agents, the context collector, and the reviewer as ordinary child-agent lifecycle items.
- Automatic reasoning summaries show scheduler activity without calling a progress tool or waking the orchestrator for each transition.
- The nested workflow completes a Luna/Sol message exchange under the repository's forced V1 multi-agent setting.
- The AST-grep preflight reports the intentional `console.log` in `flowdex-test/fixtures/ast-target.js`; the workflow treats that finding as success evidence.
- `use-context`, `parallel-note`, and `review-repair` become ready together. The context collector may delay only `use-context`; unrelated ready work should proceed.
- The reviewer reports `answer: red`, Flowdex attributes it to the worker, and the worker repairs it to `answer: blue` without routing the finding through the parent model.
- The dynamically queued task starts only after its three dependencies complete.
- Task changes are committed from isolated worktrees and integrated into this checkout.

## Validate the result

The terminal workflow output must report:

```text
result.status = "completed"
messaging.lunaStatus = "completed"
messaging.solStatus = "completed"
astRule.passed = false
astRule.expectedFinding = true
astRule.findings >= 1
queuedTaskId is a non-empty opaque string
```

Confirm these exact files:

```text
flowdex-test/results/context-result.md   -> context token: cobalt
flowdex-test/results/parallel-result.md  -> parallel branch: complete
flowdex-test/results/review-target.md    -> answer: blue
flowdex-test/results/dynamic-result.md   -> dynamic task: complete
```

Finally:

- Run `scan_flowdex_rule_candidates({})` once to confirm the read-only candidate scan returns normally. Its candidate list may be empty because this fixture marks the Markdown review defect as unsuitable for AST-grep.
- Compare final `git status --short` with the recorded baseline.
- Report the workflow result, observed signal, app-visible child/progress behavior, result-file contents, new integration commits, and any leftover registered or locked Flowdex worktree.
