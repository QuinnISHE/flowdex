# Flowdex Implementation Plan 014: Windows App Backend Installer

## Outcome

A compiled Flowdex CLI can configure the current Windows user so the Codex desktop app launches that binary as its backend.

This work is intentionally parallel to Batch 013. It owns CLI/Windows installer files and must not modify Flowdex scheduler, task, context-pack, or SQLite code.

## Command

Add:

```text
codex flowdex install --binary C:\absolute\path\to\codex.exe
```

On Windows the command:

1. Requires an absolute path.
2. Canonicalizes it and requires a regular `.exe` file.
3. Runs the supplied binary with `--version` and requires a successful Codex-identifying response before changing user state.
4. Writes the canonical path to the current user's `CODEX_CLI_PATH` environment value.
5. Reports the configured path and tells the user to restart the Codex app.

The installed Codex app's packaged runtime confirms that `CODEX_CLI_PATH` is the direct backend binary override on Windows. `CODEX_APP_SERVER_FORCE_CLI` is not required for the local Windows path and must not be written.

Use the current-user environment registry location, never machine-wide state. Prefer an existing repository Windows environment helper; otherwise add the smallest native registry implementation needed. Do not shell out to `setx`, because it obscures errors and has value-length behavior that is unnecessary here.

The command is idempotent: running it again replaces only the current user's `CODEX_CLI_PATH` value. Do not add uninstall, rollback history, PATH modification, binary copying, compilation, downloading, or app restart automation.

On non-Windows platforms, parse the same command and return a clear unsupported-platform error without changing anything.

## CLI placement

- Add a top-level `flowdex` CLI command with an `install` subcommand; keep room in the enum for future real Flowdex commands without inventing any now.
- Keep Windows registry code in a narrow platform module rather than mixing it into argument parsing.
- Reuse the repository's native path types for path-bearing arguments and resolved paths.
- Document the command in the Flowdex documentation index and a short installer page.

## Implementation ownership

Use one implementation worker for the CLI surface and Windows helper. This batch is already parallel with the context-pack thread, so do not create extra workers unless the code reveals a genuinely separate documentation task. The worker must commit its scoped work with a brief summary and must not touch scheduler-owned files.

## Verification

- Focused argument/path validation for relative paths, missing files, directories, non-`.exe` files, and a failing/non-Codex `--version` response.
- A focused Windows helper test that targets an isolated test registry location or injected registry writer; never alter the developer's real `HKCU\Environment` during tests.
- CLI parsing plus non-Windows unsupported behavior where applicable.
- Formatting and the narrow CLI checks needed for the changed files.

One cohesive review is warranted because the command mutates persistent user configuration. Review only current-user targeting, exact `CODEX_CLI_PATH` behavior, path validation, and avoidance of unintended registry/PATH changes. Do not run the full Flowdex scheduler suite for this isolated command.

## Non-goals

- No compatibility environment variables or feature flags.
- No machine-wide installation.
- No binary build/copy/download manager.
- No app GUI changes.
- No automatic app termination or restart.
