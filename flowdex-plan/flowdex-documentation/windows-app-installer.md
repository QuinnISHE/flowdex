# Flowdex desktop app installer

Place the release binary in the Flowdex package as `flowdex.exe` on Windows or `flowdex` on macOS, then run:

```text
flowdex install
flowdex uninstall
flowdex uninstall --purge
```

Install validates the running package with `--version`, copies it to
`$CODEX_HOME/flowdex/bin/codex[.exe]`, and configures the platform automatically.
It does not overwrite the Codex app or its bundled backend. On Windows the copied
path becomes the current user's `CODEX_CLI_PATH` environment value. On macOS it
is written in one managed login-profile block: zsh uses
`~/.zprofile`, bash uses `~/.bash_profile`, and fish uses
`~/.config/fish/conf.d/flowdex.fish`. Running install again replaces the managed
binary and value. Uninstall removes both. Fully quit and restart the Codex app
after either command.

The installer does not modify `PATH`, machine-wide environment state, launchd,
the Codex application bundle, or compatibility variables. The executable embeds
and creates these files when they are missing:

- `$CODEX_HOME/flowdex.toml` with documented global defaults
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
