# Native context compaction

Codex model turns may request compaction of their current conversation with the
direct model tool:

```text
compact_context({})
```

The call accepts an object with no properties; additional properties are
rejected. It returns the acknowledgement `Context compaction scheduled.`

The request applies only to the thread whose model called the tool. After the
current response and tool output finish, Codex compacts that thread at the
post-tool turn boundary. Execution then continues in the same turn: the next
model inference resumes from the compacted history.

This is a direct model tool, including for sub-agent model turns. Saved Flowdex
JavaScript workflows cannot call it, and it is not available through ordinary
`functions.exec` or general code-mode nested tools.

## Context-growth reminder

Flowdex can add one reminder when the active context reaches the configured
token threshold. The supported setting is:

```toml
compaction_reminder_threshold_tokens = 185000
```

Configuration is read from the global `$CODEX_HOME/flowdex.toml`. A trusted
project may override it in `<project-root>/.flowdex/config.toml` when that file
contains the field. The project root is the resolved Git/worktree root, or the
resolved session working directory when no Git root exists. The repository file
participates only under Codex's existing project-trust decision. The default is
`185000`; the value must be a positive integer. Missing files are normal, while
malformed TOML, unknown fields, and invalid values produce path-specific
configuration errors.

When active context tokens are at or above the resolved threshold, the next
ordinary inference receives this developer context:

```text
Your context window is growing. At the next natural task boundary, call compact_context.
```

The reminder is recorded and persisted before that inference, appears at most
once per existing context window in a process, and does not create an inference
or a user-visible message. After manual or automatic compaction, the new window
and its active-token count are evaluated naturally. Reminder delivery state is
in-memory, so a resumed process may remind again. Automatic compaction behavior
and the `compact_context` tool are unchanged.

There is no enable flag, custom text, percentage mode, hot reload,
environment/per-agent override, durable reminder bookkeeping, generic config
framework, compatibility shim, feature flag, or placeholder in this contract.
