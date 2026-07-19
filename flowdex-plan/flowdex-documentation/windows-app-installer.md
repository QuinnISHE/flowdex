# Flowdex desktop app installer

Configure the Codex desktop app to use a local Codex executable:

```text
codex flowdex install --binary C:\absolute\path\to\codex.exe
```

On macOS, use a native executable path:

```text
codex flowdex install --binary /absolute/path/to/codex
```

The command requires an absolute path to an existing regular executable,
canonicalizes it, and runs the binary with `--version`. Only a successful
Codex-identifying response allows persistent configuration. On Windows the
canonical path replaces the current user's `CODEX_CLI_PATH` environment value.
On macOS it replaces one managed block in the login-shell profile: zsh uses
`~/.zprofile`, bash uses `~/.bash_profile`, and fish uses
`~/.config/fish/conf.d/flowdex.fish`. Running it again replaces only that
managed value. Fully quit and restart the Codex app after installation.

The installer does not modify `PATH`, machine-wide environment state, launchd,
or compatibility variables. Unsupported shells and platforms fail without
mutation.
