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

This batch adds only model-requested compaction. Token thresholds, reminder
injection, and related automatic prompting behavior are future work and are
not part of the current contract.
