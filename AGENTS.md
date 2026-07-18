# Flowdex fork guidance

- Prefer direct, focused implementations that reuse existing Codex runtime seams.
- Do not add compatibility shims or feature flags unless explicitly requested.
- Keep new orchestration code out of `codex-core` when a dedicated crate is suitable.
- Add focused tests for behavior changes; keep Flowdex documentation current.
- Preserve unrelated user and worker changes. Stage only owned paths and use a brief worker commit summary.
