# Flowdex review findings and rule candidates

Review findings that meet AST-grep suitability and carry a non-empty rule key can become candidates after enough resolved occurrences. The configured `ast_grep_candidate_threshold` defaults to `3` positive occurrences; the global value in `$CODEX_HOME/flowdex.toml` is replaced by a value in a trusted repository's `.flowdex/config.toml`, and omission preserves the global value.

`scan_flowdex_rule_candidates({})` is a user-started, direct-model-only, read-only scan. It derives candidates from durable review findings and resolutions, counting each distinct finding only when its integrated commit is resolved. The result is stable and bounded to 50 candidates, with the full `resolvedOccurrences` count and no more than three exact evidence examples per candidate. Existing approved native YAML rule IDs are filtered out.

The scan does not create candidate records, write files, dispatch agents or skills, call a model, or track approval state. A user must inspect and explicitly approve each exact proposal before ordinary repository editing or dispatch may draft and write `.flowdex/ast-grep/rules/*.yml`; adding the rule to `.flowdex/config.toml` for always-on execution also requires that approval. Written rules remain native YAML and use the existing validation and execution paths.
