# Flowdex package

Place the release package files in this directory:

- Windows: copy `codex-rs/target/release/codex.exe` here as `flowdex.exe`, plus
  `codex-windows-sandbox-setup.exe` and `codex-command-runner.exe` from the same
  release directory. Flowdex installs all three together so sandboxed agents work.
- macOS: copy `codex-rs/target/release/codex` here as `flowdex` and keep it executable.

Install or uninstall from this directory:

```text
flowdex install
flowdex uninstall
flowdex uninstall --purge
```

PowerShell may require `./flowdex.exe`; macOS shells may require `./flowdex`.

Install copies the backend to `$CODEX_HOME/flowdex/bin/codex[.exe]`, configures the Codex app to use it, and creates these missing global assets from the binary:

- `$CODEX_HOME/flowdex.toml`
- `$CODEX_HOME/flowdex/workflows/defaults/{research-rounds,worker-reviewer}.js`
- `$CODEX_HOME/skills/{collect-flowdex-context,report-flowdex-review,run-flowdex-workflows}/`

Existing config, workflow, and skill files are never overwritten. The package itself can then be moved or archived. Keep a copy if you want the same `flowdex uninstall` command later.

Uninstall removes the copied backend and app override while preserving all user data and global assets. Add `--purge` to also remove only the global config, default workflows, and skills that this installer created. Pre-existing or additional global files, Flowdex runtime history, and repository-owned `.flowdex/` directories are preserved.

## Automatic upstream releases

On a GitHub fork, `Flowdex upstream release` checks the latest stable `openai/codex` release daily. A clean upstream merge produces Windows and Intel/Apple Silicon macOS packages, opens a source-sync pull request, and publishes a matching Flowdex release. A merge conflict stops the release and opens an issue for manual resolution.

Enable read/write workflow permissions and allow Actions to create pull requests in the fork's Actions settings. The workflow is disabled automatically in `openai/codex` itself.
