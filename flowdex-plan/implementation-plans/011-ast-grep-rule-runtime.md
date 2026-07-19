# Flowdex Implementation Plan 011: AST-grep Rule Runtime

## Outcome

Flowdex can execute approved repository AST-grep rules as silent verification. Repository configuration selects rules that run automatically, while saved JavaScript workflows can request specific rules directly. Violations return bounded structured findings and fail verification without waking a model.

This is the final rule-execution seam. A later review-history slice may propose and, after individual human approval, write rules into this same format; it must not replace this runtime.

## Starting point and ownership

- Start from accepted Batch 009 commit `e8e35a1e0b78f05f791677171c70063b4e7ffb50` in a separate worktree.
- Batch 010 is concurrently changing the scheduler, Flowdex bootstrap, and store. Keep this branch independent and make small integration commits so the planner can apply it after Batch 010.
- `codex-flowdex` remains independent of `codex-core` and owns rule discovery, parsing, selection, execution, and result types.
- `codex-core` owns the hidden saved-workflow bridge and integration with the existing verification lifecycle.
- Do not modify the Batch 010 scheduler, run/phase/task store schema, progress machinery, or task scheduling policy.

## Final repository and configuration contract

Approved rules live beneath:

```text
.flowdex/ast-grep/rules/*.yml
```

Use ordinary AST-grep YAML rule files. The rule's native `id` is the stable Flowdex rule key. Reject duplicate IDs and invalid rule files with a path-specific bounded error.

Extend the existing strict Flowdex configuration with one setting:

```toml
ast_grep_always_run = ["rule-id", "another-rule"]
```

- Default is an empty list.
- Global `$CODEX_HOME/flowdex.toml` loads first. A present trusted-repository value in `.flowdex/config.toml` replaces it; omission preserves the global value.
- Reject empty IDs and duplicates.
- Rules are always loaded from the trusted repository's `.flowdex/ast-grep/rules/` directory. Configuration cannot supply arbitrary paths.
- Missing referenced rules are configuration/runtime errors, not passing verification.
- Do not add enable flags, alternate rule directories, globs, language registries, compatibility aliases, or a generic configuration framework.

## Execution behavior

Use AST-grep's Rust libraries in-process. Do not require Node, npm, a separately installed `ast-grep` executable, or shell command construction.

The `codex-flowdex` rule runner receives an already eligible trusted repository root, an execution root, and exact rule IDs. It:

1. Resolves rule files strictly beneath the repository rule directory.
2. Parses and compiles the selected native rules.
3. Scans files beneath the execution root using the rule languages and normal AST-grep ignore behavior where available.
4. Returns stable structured findings ordered by rule ID, repository-relative file, line, and column.

Each finding contains only:

- `ruleId`
- `file`
- `line`
- `column`
- `message`
- `severity`

Paths are repository-relative. Line and column are one-based. Bound finding count and serialized text using existing Flowdex/code-mode output limits; report truncation explicitly rather than silently dropping evidence.

Do not permit rules to execute fixes or rewrite files in this slice.

## Saved-workflow API

Add one hidden Flowdex-only primitive:

```js
const result = await flowdex.checkRules(["rule-id", "another-rule"]);
```

- The argument is one non-empty array of unique non-empty rule IDs.
- The wrapper rejects extra arguments and invalid values.
- The result is:

```js
{
  passed: false,
  findings: [
    {
      ruleId: "rule-id",
      file: "src/parser.rs",
      line: 42,
      column: 9,
      message: "...",
      severity: "warning",
    },
  ],
  truncated: false,
}
```

- `findings` is empty when passing. `truncated` is present only when true if that matches existing Flowdex optional-field conventions.
- It uses the workflow's current execution root, including a task worktree when invoked there.
- It is not a direct model tool, ordinary `functions.exec` tool, or general code-mode nested tool.
- It emits normal tool lifecycle events but no intermediate model turn and no passing output into parent model context.

## Automatic verification integration

After configured command verification passes, run `ast_grep_always_run` against the same execution root before reporting overall success.

- A rule violation changes the overall verification result to `passed: false` and includes the structured rule findings.
- Command results retain their settled Batch 003 shape and behavior.
- Command failure or timeout remains first-failure and does not run the rule stage.
- Denial, cancellation, invalid configuration, invalid rules, and internal errors remain tool errors.
- Preserve task worktree cwd/roots, sandbox and approval behavior for commands, hooks, selected execution backend, cancellation teardown, bounded output, and exact-HEAD task verification.
- Rust does not choose a repair agent or start review. Later scheduler policy consumes the failure.

Extend the existing verification result only with an optional `rules` result when the automatic rule stage actually runs. Do not rename or remove existing fields.

## Implementation sequence and parallel work

The orchestrator uses `gpt-5.6-sol` on low reasoning and may dispatch these two workers in parallel after agreeing on the small exported result contract:

1. **Rule engine and configuration — `implementation_worker`**
   - Own `codex-rs/flowdex/src/ast_grep.rs`, `codex-rs/flowdex/src/config.rs`, crate manifests, and focused `codex-flowdex` tests.
   - Add native rule loading/execution, result types, strict configuration, bounds, and containment.

2. **Core bridge and workflow API — `implementation_worker`**
   - Own a new focused core Flowdex rule handler, minimal registration/bootstrap edits, verification-stage integration, and one cohesive core integration test.
   - Reuse the agreed `codex-flowdex` contract. Do not duplicate parsing or scanning in core.

After integration, a fast documentation worker may document the rule directory, configuration, JavaScript call, and result shape in `flowdex-plan/flowdex-documentation/` and link it from the index.

Workers use managed worktrees, do not revert one another's changes, and commit their scoped work with a brief summary. Keep rule-engine, core-integration, and documentation commits separable for later application onto Batch 010.

## Proportionate verification and review

Implementation is the priority. Verification should prove the real cross-crate behavior without building a test matrix:

- One focused `codex-flowdex` test exercises native rule loading, exact selection, a real match, stable location output, and strict configuration.
- One cohesive core workflow test proves an approved always-on rule runs in a task worktree, a violation fails verification without a model turn, and an explicit `checkRules` call uses the same engine.
- Run formatting, scoped fixes, and checks only for touched crates.
- Do not add separate tests for every invalid string, optional omission, language, severity, or truncation branch unless implementation exposes a concrete regression risk.

After the complete path works, use one `gpt-5.6-luna` xhigh reviewer. Ask it to focus on repository containment/trust, native rule execution, verification semantics, cancellation, bounded results, hidden visibility, and overlap with Batch 010. Fix actionable findings and rerun only affected checks. Do not dispatch multiple independent reviewers or expand optional suggestions into unrelated work.

## Deferred

Do not add in this batch:

- reviewer report persistence or semantic clustering;
- repeated-resolution detection or candidate promotion;
- rule-writing agents or human approval UI;
- automatic fixes;
- scheduler repair/review rounds;
- generic/nested workflows, context packs, tool profiles, boundaries, or installer behavior;
- compatibility shims, feature flags, or placeholders.

## Completion report

Report commits, exact rule/config/API contracts, focused checks, reviewer findings and fixes, Batch 010 overlap, and a clean worktree. Send the completion delegation to planner task `019f7311-1c7a-7fb3-b06c-7e96991efeec`. Do not design the next plan or wait for a response. Compact context at the batch boundary.
