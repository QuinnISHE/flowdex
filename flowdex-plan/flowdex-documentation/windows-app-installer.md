# Flowdex desktop app installer

Extract the release archive. The Windows package contains `flowdex.exe`,
`codex-windows-sandbox-setup.exe`, and `codex-command-runner.exe`; both helpers
must remain beside `flowdex.exe` during installation. A macOS package contains
`flowdex`. Then run:

```text
flowdex install
flowdex uninstall
flowdex uninstall --purge
```

Install validates the running package with `--version`, copies it to
`$CODEX_HOME/flowdex/bin/codex[.exe]`, and configures the platform automatically.
It does not overwrite the Codex app or its bundled backend. On Windows the copied
path becomes the current user's `CODEX_CLI_PATH` environment value. On macOS it
installs `~/Library/LaunchAgents/com.openai.flowdex-codex-cli-path.plist` to
restore `CODEX_CLI_PATH` for each GUI login and also applies the value to the
current `launchctl` session. Running install again replaces the managed binary,
LaunchAgent, and current-session value. Uninstall unloads and removes the
LaunchAgent and unsets the current-session value. Fully quit and restart the
Codex app after either command.

The installer does not modify `PATH`, shell profiles, machine-wide environment
state, the Codex application bundle, or compatibility variables. The executable embeds
and creates these files when they are missing:

- `$CODEX_HOME/flowdex.toml` with every global option populated at its default (`185000` compaction reminder tokens, multi-agent V1, AST-grep candidate threshold `3`, no always-run rules, and no tool profiles). Reinstall adds missing options to a valid existing config without replacing existing values.
- `$CODEX_HOME/flowdex/workflows/defaults/research-rounds.js`
- `$CODEX_HOME/flowdex/workflows/defaults/worker-reviewer.js`
- each skill's `SKILL.md` and `agents/openai.yaml` under
  `$CODEX_HOME/skills/{collect-flowdex-context,report-flowdex-review,run-flowdex-workflows}/`

Install never overwrites an existing config, workflow, or skill file. It records
only the files it creates in `$CODEX_HOME/flowdex/installed-assets-v1`. Normal
uninstall preserves all global assets and user data. `uninstall --purge` removes
the installer-owned files recorded there, but preserves pre-existing or
additional global files, runtime history, task worktrees, and repository
`.flowdex/` directories.
