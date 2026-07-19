# Flowdex Implementation Plan 014: Desktop App Backend Installer

## Outcome

A compiled Flowdex CLI can configure the current Windows or macOS user so the Codex desktop app launches that binary as its backend.

This work is intentionally parallel to Batch 013. It owns CLI and platform installer files and must not modify Flowdex scheduler, task, context-pack, or SQLite code.

## Command

Add:

```text
codex flowdex install --binary C:\absolute\path\to\codex.exe
```

The macOS spelling is the same command with a native path:

```text
codex flowdex install --binary /absolute/path/to/codex
```

On Windows the command:

1. Requires an absolute path.
2. Canonicalizes it and requires a regular `.exe` file.
3. Runs the supplied binary with `--version` and requires a successful Codex-identifying response before changing user state.
4. Writes the canonical path to the current user's `CODEX_CLI_PATH` environment value.
5. Reports the configured path and tells the user to restart the Codex app.

The installed Codex app's packaged runtime confirms that `CODEX_CLI_PATH` is the direct backend binary override on Windows. `CODEX_APP_SERVER_FORCE_CLI` is not required for the local Windows path and must not be written.

Use the current-user environment registry location, never machine-wide state. Prefer an existing repository Windows environment helper; otherwise add the smallest native registry implementation needed. Do not shell out to `setx`, because it obscures errors and has value-length behavior that is unnecessary here.

On macOS the command:

1. Requires an absolute path.
2. Canonicalizes it and requires a regular executable file. Do not require an `.exe` suffix.
3. Runs the supplied binary with `--version` and applies the same Codex-identifying check before changing user state.
4. Persists `CODEX_CLI_PATH` in the current user's login-shell configuration.
5. Reports the configured path and tells the user to fully quit and restart the Codex app.

The desktop app runtime reads `CODEX_CLI_PATH` on both `darwin` and `win32`. On macOS it imports the user's interactive login-shell environment into the app process before resolving the backend. A non-empty override also bypasses the optional local-daemon path, so `CODEX_APP_SERVER_FORCE_CLI` is not needed and must not be written.

Use that existing app behavior instead of installing a LaunchAgent or relying on the session-only effect of `launchctl setenv`:

- Resolve the current login shell from `SHELL` and support the normal macOS shells directly: zsh uses `~/.zprofile`, bash uses `~/.bash_profile`, and fish uses `~/.config/fish/conf.d/flowdex.fish`.
- Maintain one clearly marked Flowdex block containing the correctly quoted canonical path. Replace that block on repeated installation and preserve all unrelated profile content. Reject malformed or duplicate Flowdex markers instead of guessing.
- Use POSIX `export` syntax for zsh/bash and fish `set -gx` syntax for fish. Quote paths as data; never construct a command that can execute path contents.
- Write through a same-directory temporary file and atomic replacement. Preserve existing file permissions when replacing a profile and use ordinary user-only writable defaults for a new file.
- If the login shell is unsupported or cannot be identified, return a clear error without changing a profile. Do not silently edit a profile the shell will not load.

The command is idempotent: running it again replaces only the current user's `CODEX_CLI_PATH` registry value or managed shell-profile block. Do not add uninstall, rollback history, PATH modification, binary copying, compilation, downloading, or app restart automation.

On platforms other than Windows and macOS, parse the same command and return a clear unsupported-platform error without changing anything.

## CLI placement

- Add a top-level `flowdex` CLI command with an `install` subcommand; keep room in the enum for future real Flowdex commands without inventing any now.
- Keep Windows registry and macOS shell-profile code in narrow platform modules rather than mixing them into argument parsing.
- Reuse the repository's native path types for path-bearing arguments and resolved paths.
- Document the command in the Flowdex documentation index and a short installer page.

## Implementation ownership

Use one implementation worker for the CLI surface and platform helpers. This batch is already parallel with the context-pack thread, so do not create extra workers unless the code reveals a genuinely separate documentation task. The worker must commit its scoped work with a brief summary and must not touch scheduler-owned files.

## Verification

- Focused argument/path validation for relative paths, missing files, directories, non-`.exe` files, and a failing/non-Codex `--version` response.
- A focused Windows helper test that targets an isolated test registry location or injected registry writer; never alter the developer's real `HKCU\Environment` during tests.
- Focused pure tests for macOS shell selection, safe zsh/bash/fish quoting, marker insertion/replacement, malformed-marker rejection, and preservation of unrelated profile bytes. Tests must use an injected filesystem and must not alter the developer's real profiles or launch environment.
- The macOS implementation cannot be exercised against a real Codex app in this Windows workspace. Keep platform operations behind a small injectable seam and verify compile-time `cfg` coverage where practical; document the missing live macOS run without pretending unit coverage proves it.
- CLI parsing plus unsupported-platform behavior where applicable.
- Formatting and the narrow CLI checks needed for the changed files.

One cohesive review is warranted because the command mutates persistent user configuration. Review only current-user targeting, exact `CODEX_CLI_PATH` behavior, safe profile preservation/quoting, path validation, and avoidance of unintended registry/profile/PATH changes. Do not run the full Flowdex scheduler suite for this isolated command.

## Non-goals

- No compatibility environment variables or feature flags.
- No machine-wide installation.
- No LaunchAgent, login item, or temporary-only `launchctl setenv` installation.
- No binary build/copy/download manager.
- No app GUI changes.
- No automatic app termination or restart.
