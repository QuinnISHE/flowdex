# Flowdex Batch 011: AST-grep rules

Flowdex runs approved repository AST-grep rules in-process as silent verification. It does not require Node, npm, an `ast-grep` executable, or shell command construction, and rules cannot apply fixes or rewrite files.

## Rules and configuration

Native AST-grep YAML rules live beneath `.flowdex/ast-grep/rules/*.yml`. The YAML `id` is the stable Flowdex rule ID. Duplicate IDs, invalid rules, missing selected rules, and rules that define fixes or rewriters are errors. Rule files are always loaded from the trusted repository; configuration cannot select another directory.

The strict Flowdex configuration accepts:

```toml
ast_grep_always_run = ["rule-id", "another-rule"]
```

The default is empty. `$CODEX_HOME/flowdex.toml` loads first. When trusted repository `.flowdex/config.toml` contains this setting, its value replaces the global list; omission preserves the global list. IDs must be unique and non-blank.

## Candidate scans and promotion

Flowdex can derive rule candidates from its durable review history. Configure the minimum number of resolved occurrences with:

```toml
ast_grep_candidate_threshold = 3
```

The threshold is a positive integer and defaults to `3`. `$CODEX_HOME/flowdex.toml` loads first; a value in a trusted repository's `.flowdex/config.toml` replaces it, while omission preserves the global value. It is loaded with the rest of Flowdex configuration and is not a scan argument.

The user-started direct-model tool `scan_flowdex_rule_candidates({})` returns candidates in stable rule-key order, bounded to 50 candidates. A candidate is based on distinct findings that are AST-grep-suitable, have a non-empty rule key, and have a recorded resolution with an integrated commit. `resolvedOccurrences` counts the complete eligible history; each candidate includes at most three deterministic exact evidence examples with `file`, `lineStart`, `lineEnd`, `reason`, `sourceCommit`, and `integratedCommit`. The durable findings and resolutions remain the source of truth; Flowdex does not persist a candidate table. A candidate whose exact native YAML rule ID is already approved in the repository is excluded.

Scanning is read-only and does no model work, dispatch, writing, configuration mutation, or approval-state tracking. After inspecting a candidate, the user may explicitly confirm that exact proposal. Only then may ordinary repository editing or an agent/skill draft and write a native YAML rule (and, if requested, add its ID to `ast_grep_always_run`). Approved rules continue through the existing YAML validation, explicit `flowdex.checkRules(...)`, and always-on verification paths.

## Saved-workflow API

Saved JavaScript workflows can request exact rules:

```js
const result = await flowdex.checkRules(["rule-id", "another-rule"]);
```

`checkRules` accepts exactly one non-empty array of unique, non-blank string IDs. It is hidden from models, ordinary `functions.exec`, and general code-mode nested tools. It uses the workflow's current execution root and emits normal tool lifecycle events without an intermediate model turn.

The result is:

```js
{
  passed: false,
  findings: [{
    ruleId: "rule-id",
    file: "src/parser.rs",
    line: 42,
    column: 9,
    message: "...",
    severity: "warning",
  }],
  truncated: true,
}
```

`file` is relative to the execution root and locations are one-based. Findings are ordered by rule ID, file, line, and column. `truncated` is omitted unless evidence was bounded.

## Automatic verification

After configured command verification succeeds, Flowdex runs `ast_grep_always_run` against the same execution root, including task worktrees. A finding changes the overall verification result to `passed: false` and adds `rules` with the result above. Command failure or timeout remains first-failure and skips rules. Invalid configuration or rules, denial, cancellation, traversal failures, and internal failures remain tool errors.

Task verification loads rules from the trusted base repository, validates that the task worktree belongs to that repository, scans the worktree, and preserves exact-HEAD verification. Results and errors are bounded and scans observe cancellation.

## Current limits

Candidate scanning does not automatically create, promote, enable, or modify rules; choose repair or review agents; add alternate rule directories, globs, language registries, enable flags, or compatibility aliases; or change scheduler policy. Every rule or always-on configuration edit requires explicit user approval of that individual proposal.
