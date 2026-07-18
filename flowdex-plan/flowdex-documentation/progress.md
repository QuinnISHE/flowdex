# Flowdex Batch 004: Progress summaries

Saved workflows do not expose a callable progress API. Scheduler transitions
publish concise, transient updates through Codex's existing reasoning-summary UI path.

## Transient behavior and limits

Each accepted call appears live as one completed existing reasoning item for
current UI subscribers. It is UI-only and transient: it is not persisted in
rollout or conversation history, is not included in a later model request, and
is not returned through the parent workflow result. Consequently, summaries
cannot be recovered after a workflow is resumed.

The scheduler owns these summaries and emits them automatically at task, phase,
and workflow transitions. They are not callable workflow methods or durable
progress-history APIs.
