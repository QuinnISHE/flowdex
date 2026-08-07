Integrate the checked-out official Codex release into Flowdex.

The repository may be in the middle of a merge with conflicts. Resolve every conflict and adapt Flowdex to the current upstream APIs. Preserve both the upstream behavior and all Flowdex functionality. In particular, protect Flowdex's workflow runtime, agent lifecycle and app events, scheduler, worktrees and permissions, verification, context packs, review routing, compaction, installer, skills, and SQLite state.

Use the upstream implementation as the source of truth for changed Codex APIs, then fit Flowdex into those APIs. Inspect each conflict and nearby call sites. Do not resolve the repository wholesale with `ours` or `theirs`, abort the merge, remove Flowdex behavior, disable checks, or add compatibility shims, feature flags, or placeholders.

Keep the change focused on integration. Run `git diff --check`, format changed Rust files, and run the narrowest useful checks that fit the available time. Leave the working tree with no unresolved conflicts. Do not commit or push; the workflow handles that.
