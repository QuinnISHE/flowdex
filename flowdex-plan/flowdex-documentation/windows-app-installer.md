# Flowdex Windows app installer

On Windows, configure the Codex desktop app to use a local Codex executable:

```text
codex flowdex install --binary C:\absolute\path\to\codex.exe
```

The command requires an absolute path to an existing regular `.exe` file. It
canonicalizes the path, runs the binary with `--version`, and only after a
successful Codex-identifying response writes the canonical path to the
current user's `CODEX_CLI_PATH` environment setting. Running it again replaces
that value. Restart the Codex app after installation.

The installer does not modify `PATH`, machine-wide environment state, or any
other compatibility variables. It is unavailable on non-Windows platforms.
