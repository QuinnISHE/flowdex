# Flowdex package

Place one release binary in this directory:

- Windows: copy `codex-rs/target/release/codex.exe` here as `flowdex.exe`.
- macOS: copy `codex-rs/target/release/codex` here as `flowdex` and keep it executable.

Install or uninstall from this directory:

```text
flowdex install
flowdex uninstall
flowdex uninstall --purge
```

PowerShell may require `./flowdex.exe`; macOS shells may require `./flowdex`.

Install copies the backend to `$CODEX_HOME/flowdex/bin/codex[.exe]` and configures the Codex app to use it. The package itself can then be moved or archived. Keep a copy if you want the same `flowdex uninstall` command later.

Uninstall removes the copied backend and app override. It preserves workflows, optional configuration, and Flowdex runtime history. Add `--purge` to also remove `$CODEX_HOME/flowdex/` and `$CODEX_HOME/flowdex.toml`. Repository-owned `.flowdex/` directories are preserved because they may be committed project files.

## Automatic upstream releases

On a GitHub fork, `Flowdex upstream release` checks the latest stable `openai/codex` release daily. A clean upstream merge produces Windows and Intel/Apple Silicon macOS packages, opens a source-sync pull request, and publishes a matching Flowdex release. A merge conflict stops the release and opens an issue for manual resolution.

Enable read/write workflow permissions and allow Actions to create pull requests in the fork's Actions settings. The workflow is disabled automatically in `openai/codex` itself.
