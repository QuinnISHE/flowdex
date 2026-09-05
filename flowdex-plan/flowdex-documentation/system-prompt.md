# System prompt mode

Flowdex can use Codex's normal model-catalog instructions, a combined Claude Code client prompt port, or Pi's compact coding-agent prompt:

```toml
system_prompt_mode = "codex" # or "claude" or "pi"
```

Set the value in `$CODEX_HOME/flowdex.toml`. A trusted repository may override it in `.flowdex/config.toml`; repository omission retains the global value. `codex` is the default. Restart Codex and begin a new task after changing it.

All three modes identify the harness as Codex. Flowdex names the workflow/runtime extensions; it is not the agent's product identity.

## Claude-mode source

The original source was the sanitized Claude Code 2.1.198 prompt-excerpt archive supplied for this project. The implementation is now aligned to the locally installed Claude Code 2.1.234 client and the matching extracted [`v2.1.234` client prompt corpus](https://github.com/Piebald-AI/claude-code-system-prompts/tree/v2.1.234) at commit `373b98c7a1a57f36e97a825f3297cd0a00f7d4d6`.

The archive contains exact client-binary excerpts, not a captured Anthropic API request. Claude Code also assembles conditional and dynamic sections at runtime. The corpus has hundreds of prompts for utility agents, tools, commands, reports, classifiers, and feature-specific modes; Claude Code does not concatenate them into one request. Flowdex combines the complete recoverable primary interactive path with the coordinator and worker paths. Codex continues to supply runtime-only conditions through its native developer context and tool schemas.

The port copies the source wording directly and limits changes to these required integration points:

| Source section | Required Codex change |
| --- | --- |
| Interactive identity | Adds `You are Codex`; never identifies the harness as Flowdex or Claude |
| Harness output | Replaces the terminal-only display statement with the Codex interface and maps injected reminder wording to Codex system/developer context |
| Action safety | Replaces durable `CLAUDE.md` authorization with Codex's `AGENTS.md` |
| Hooks | Maps Claude's prompt-submit tag to Codex hook feedback without changing the policy |
| Coordinator tools | Replaces Claude's `Agent`, `SendMessage`, `TaskStop`, listing, and wait names with `spawn_agent`, `followup_task`, `send_message`, `interrupt_agent`, `list_agents`, and `wait_agent` |
| Workflows | Maps Claude workflow orchestration to the `run-flowdex-workflows` skill and saved Flowdex JavaScript APIs, including event-driven waits, tasks, context packs, verification, reviews, and boundaries |
| Worker roles | Keeps Claude's coordinator-worker instructions and adds only the exact Flowdex publication/report obligations supplied to context collectors and reviewers |
| Dynamic tool and environment data | Leaves schemas, permissions, cwd, platform, repository status, skills, and project instructions in Codex's existing dynamic context instead of freezing stale copies into the prompt |

This is deliberately a prompt set, not one indiscriminate concatenation of the corpus. Utility-agent prompts such as compaction summarizers, permission classifiers, artifact composers, insights generators, and command-specific helpers remain outside the ordinary coding-agent request unless Codex invokes its own equivalent feature.

Claude model-family identity, Anthropic account/subscription text, Claude feedback URLs, and unavailable Claude-only services are not asserted. Tool descriptions remain dynamically supplied by Codex's real schemas, matching Claude Code's own separation between system sections and tool definitions.

## Exact source map

The invariant prompt in `codex-rs/flowdex/src/claude_system_prompt.md` is assembled from these v2.1.234 sections:

- `system-prompt-harness-instructions.md`
- `system-prompt-system-section.md`
- `system-prompt-communication-style.md`
- `system-prompt-action-safety-and-truthful-reporting.md`
- `system-prompt-task-approval-continuity.md`
- `system-prompt-autonomous-operation-guidelines.md`
- `system-prompt-act-when-ready.md`
- `system-prompt-delivering-work-at-full-scope.md`
- `system-prompt-correction-restraint.md`
- the strings returned by `03-full-coding-instructions-Qfm.js.txt`
- `system-prompt-emoji-avoidance.md`
- `system-prompt-hook-feedback-handling.md`
- `system-prompt-parallel-tool-call-note-part-of-tool-usage-policy.md`
- `system-prompt-subagent-delegation-restraint.md`

The root suffix comes from `system-prompt-coordinator-mode-orchestration.md`. The worker suffix comes from `agent-prompt-coordinator-worker-instructions.md`. Required Codex and Flowdex substitutions are listed above; tool- and transport-independent wording is retained from those source sections.

## Assembly and prompt caching

Claude mode is assembled from immutable compile-time sections:

1. Every request begins with the same invariant base prompt.
2. A root task appends the coordinator section.
3. A newly spawned child appends the worker section.

The common prefix is byte-identical across roles. Volatile data—cwd, date, repository status, permissions, tool inventory, skills, AGENTS instructions, and user context—continues through Codex's normal dynamic developer/user context rather than being interpolated into the system prompt. This keeps system-prompt variants bounded and preserves prefix-cache reuse.

An explicit native Codex base-instructions override still takes precedence. Resumed and forked conversations retain their persisted base instructions, so changing the setting does not mutate an existing history.

## Pi mode

Pi mode is a direct adaptation of the default template in [`buildSystemPrompt`](https://github.com/earendil-works/pi/blob/209bc7b9a89b01c8fd05861cf5bbdda3e300037a/packages/coding-agent/src/core/system-prompt.ts), pinned at commit `209bc7b9a89b01c8fd05861cf5bbdda3e300037a`. The identity sentence, available-tools structure, custom-tool sentence, guideline wording, and documentation behavior are copied rather than summarized.

The required substitutions are narrow: `pi` becomes `Codex`; the dynamically generated Pi tool list becomes Codex's separately supplied native tool schemas; Pi installation paths become Codex's documentation skill and repository documentation. Pi's project context, skills, and current working directory remain in Codex's existing dynamic developer/user context instead of being duplicated in the stable system text. Root and child tasks therefore share one byte-identical Pi system prompt while still receiving their live Codex tools, AGENTS instructions, skills, permissions, and environment.

Explicit base-instruction overrides still replace Pi mode, and existing conversations retain their persisted prompt.
