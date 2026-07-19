# Flowdex Implementation Plan 017: AST-grep Candidate Promotion

## Outcome

Flowdex can turn repeated, resolved review findings into bounded repository rule candidates. A user can explicitly ask the model to scan those candidates, inspect representative evidence, and optionally dispatch a rule-writing agent or skill. Flowdex never writes or enables a rule before the user approves that exact proposal.

This completes the backend machinery requested for rule promotion. It builds on the accepted review history and AST-grep runtime; it does not introduce a second rule format, candidate database, background classifier, or automatic rule-writing loop.

## Settled Starting Point

- Start from authoritative commit `fc6a8f03b1f2a1c5ba3289eb6d1d3050edb643bc` after accepted Batch 016.
- Batch 015 already persists review findings, stable rule keys, AST-grep suitability, exact repair operations, and source/integrated repair commits in the repository Flowdex store.
- Batch 011 already owns approved YAML loading, safe rule discovery, in-process execution, explicit `flowdex.checkRules(...)`, and `ast_grep_always_run` verification.
- The repository database is already scoped by canonical Git common-directory identity. Candidate scanning is repository-wide, not limited to the current run.
- Preserve all workflow, scheduler, signal, review, attribution, worktree, verification, and app-server behavior from Batches 001-016.

## Final Configuration Contract

Add one consumed Flowdex setting:

```toml
ast_grep_candidate_threshold = 3
```

- The default is `3` resolved occurrences.
- The value must be a positive integer.
- Load the global value from `$CODEX_HOME/flowdex.toml`; a present value in a trusted repository's `.flowdex/config.toml` replaces it. Omission preserves the global value.
- Keep loading once with the existing Flowdex configuration. Do not add an enable flag, per-rule thresholds, hot reload, or another configuration file.

## Candidate Derivation

Add repository-independent `codex-flowdex` domain types for a rule candidate and its evidence, plus one store query that derives candidates from existing durable rows.

An occurrence is eligible only when:

- `review_findings.ast_grep_suitable` is true;
- `review_findings.rule_key` is present and non-empty;
- the finding has a recorded `review_resolutions` row with a non-null integrated commit.

Count each distinct finding once even if it has multiple resolution rows. Group by the reviewer-provided rule key and return only groups whose count meets the configured threshold. Do not use model-based similarity, reason-text clustering, file-name heuristics, or background processing.

Join a resolution back to `task_commits` with both `repair_operation_id = operation_id` and `source_commit`, then resolve its durable integrated commit with `COALESCE(review_resolutions.integrated_commit, task_commits.integrated_commit)`. Task repairs initially store the integrated commit only when later integration completes, so reading `review_resolutions.integrated_commit` alone would silently omit valid history. Rank multiple repair commits per finding deterministically and count only the final integrated representative.

Do not persist a second candidate table. Findings and resolutions are the source of truth, so a scan cannot become stale when another finding is resolved. Exclude a candidate when an approved repository YAML rule already has that exact native rule ID; the accepted rule directory is authoritative regardless of whether the rule is always-on or workflow-selected.

Return candidates in stable rule-key order. Bound the scan to 50 candidates and three deterministic evidence examples per candidate. Each example contains exactly:

```text
file
lineStart
lineEnd
reason
sourceCommit
integratedCommit
```

The occurrence count is computed over the complete eligible history, not the bounded examples. Reuse existing bounded error/result handling and never expose database paths, worktree paths, operation IDs, agent IDs, or raw SQLite rows.

## User-started Scan Tool

Add one direct-model-only tool:

```text
scan_flowdex_rule_candidates({})
```

Its schema is an empty object with `additionalProperties: false`. It requires a trusted Git repository and resolves that repository's existing Flowdex store and loaded candidate threshold. It returns exactly:

```json
{
  "candidates": [
    {
      "ruleKey": "avoid-unchecked-layout-cast",
      "resolvedOccurrences": 3,
      "examples": [
        {
          "file": "src/layout.rs",
          "lineStart": 42,
          "lineEnd": 44,
          "reason": "The cast bypasses the checked layout helper.",
          "sourceCommit": "...",
          "integratedCommit": "..."
        }
      ]
    }
  ]
}
```

An empty scan returns `{ "candidates": [] }`.

The tool is not available to saved workflow JavaScript, general code-mode execution, review children, or ordinary subagents. It performs no model call, agent dispatch, file write, configuration mutation, or progress event. The parent model is awake only because the user explicitly requested the scan.

## Approval and Rule-writing Flow

Document the final user-controlled flow:

1. The user asks Codex to scan for rule candidates.
2. The model calls `scan_flowdex_rule_candidates({})` and presents the candidate evidence.
3. At the user's request, an ordinary rule-writing agent or future skill inspects the integrated commits and drafts one native AST-grep YAML rule.
4. Codex shows the exact YAML and whether the proposal is explicit-only or adds its ID to repository `ast_grep_always_run`.
5. Only after the user approves that individual proposal does ordinary repository editing write `.flowdex/ast-grep/rules/*.yml` and, for always-on behavior, update `.flowdex/config.toml`.

Do not add an install/approve boolean tool, an approval ledger, a rule-writing runtime role, or a skill in this batch. Existing Codex editing and user confirmation provide the write boundary; the Flowdex scan stays read-only.

## Implementation Shape

Establish the candidate/config/result types first, commit that shared contract, then parallelize the independent work:

1. **Store and rule discovery:** implement the resolved-finding query, deterministic bounds, and exact approved-rule filtering in `codex-flowdex`.
2. **Core tool bridge:** register the strict direct-model scan, resolve trust/repository identity/config through existing Flowdex seams, and serialize the exact result.
3. **Documentation:** extend the AST-grep and review documentation with the candidate and approval flow, and update the source-of-truth index.

Use `implementation_worker` for the store/core slices and `implementation_worker_fast` for isolated documentation or mechanical wiring. Workers should commit brief scoped summaries. Integrate completed worker commits promptly rather than holding a large late reconciliation step.

If a hard-stop condition occurs, immediately message the planning thread with the exact blocker before ending the task. A clean detached worktree at the exact starting commit is authorized and is not a blocker.

## Focused Verification

Verification should prove the new derivation and trust boundary, not repeat the Batch 011 or Batch 015 suites.

- Before implementation, add focused tests that fail for the missing candidate behavior.
- In `codex-flowdex`, cover threshold/default/override validation, eligible resolved findings, unresolved or unsuitable exclusion, distinct-finding counting, deterministic example bounds, and exclusion of an already approved rule ID.
- In `codex-core`, cover the exact empty direct-model schema, trusted-repository requirement, empty result, and one serialized candidate result.
- Run the affected Flowdex crate tests, the focused core tool/integration tests, formatting, and targeted checks for changed crates. Do not run the full workspace suite unless a focused failure points to broader impact.

After the complete path works, use exactly one `gpt-5.6-luna` `xhigh` reviewer for the cohesive change. Review the store query, rule-ID filtering, trust/root selection, bounded model output, and proof that scanning cannot write or dispatch. Fix concrete findings and rerun only affected checks.

## Documentation

Update the Flowdex documentation to state:

- the threshold setting and repository/global precedence;
- the exact scan result and eligibility rules;
- candidates are derived from resolved findings rather than persisted separately;
- approved rule IDs disappear from candidate scans;
- scanning is read-only and user-started;
- individual approval precedes every repository rule/config edit;
- approved rules continue to use the Batch 011 YAML and explicit/always-on execution paths.

## Non-goals

- No semantic clustering, embeddings, background scan, automatic model wake, or scheduler integration.
- No candidate table, candidate lifecycle state, rejection memory, approval ledger, or durable proposal record.
- No automatic agent dispatch, generated rule, file write, config mutation, or rule enablement.
- No new workflow JavaScript primitive, rule format, AST-grep engine, CLI command, GUI change, compatibility shim, feature flag, or placeholder.
- No process-restart scheduler recovery or unrelated completion-pass work.
