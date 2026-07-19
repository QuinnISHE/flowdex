# Flowdex desktop app installer

Place the release binary in the Flowdex package as `flowdex.exe` on Windows or `flowdex` on macOS, then run:

```text
flowdex install
flowdex uninstall
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
the Codex application bundle, or compatibility variables. It does not create a
Flowdex config file; global and repository configuration remain optional.
Uninstall leaves workflows, optional configuration, and runtime history intact.
