# Flowdex package

## Judge demo quick start

1. Extract the complete archive and keep the four `.exe` files together.
2. In PowerShell, run `./flowdex.exe install`.
3. Fully quit and reopen the Codex app.
4. Open the included `Flowdex-Demo` folder in Codex and trust the repository.
5. Ask Codex: `Read START-HERE.md and follow it exactly.`
6. When the workflow completes, open `Flowdex-Demo/index.html` directly.

The demo begins as a clean Git repository with no generated application files.
Flowdex collects a context pack, runs a reusable nested workflow, dispatches
parallel implementation agents, dynamically queues a dependent task, runs
automatic verification and an AST-grep rule, routes an intentional review
finding back to its worker, pauses at an orchestrator boundary, and integrates
the committed result. The finished page is a centered animated FLOWDEX logo.

## Package contents

- `flowdex.exe`
- `codex-code-mode-host.exe`
- `codex-windows-sandbox-setup.exe`
- `codex-command-runner.exe`
- `Flowdex-Demo/`

For a package assembled from source, place the release files in this directory:

- Windows: copy `codex-rs/target/release/codex.exe` here as `flowdex.exe`, plus
  `codex-code-mode-host.exe`, `codex-windows-sandbox-setup.exe`, and
  `codex-command-runner.exe` from the same release directory. Flowdex installs
  all four together so code mode and sandboxed agents work.
- macOS: copy `codex-rs/target/release/codex` here as `flowdex` and
  `codex-rs/target/release/codex-code-mode-host` beside it; keep both executable.

Install or uninstall from this directory:

```text
flowdex install
flowdex uninstall
flowdex uninstall --purge
```

PowerShell may require `./flowdex.exe`; macOS shells may require `./flowdex`.

Install copies the backend to `$CODEX_HOME/flowdex/bin/codex[.exe]`, configures the Codex app to use it, and creates these missing global assets from the binary:

- `$CODEX_HOME/flowdex.toml`, populated with all global defaults (including a `185000`-token compaction reminder, five-minute verification timeout, multi-agent V1, and child tool/skill exclusions)
- `$CODEX_HOME/flowdex/workflows/defaults/{research-rounds,worker-reviewer}.js`
- `$CODEX_HOME/skills/run-flowdex-workflows/`, including standalone JavaScript examples

Existing config values, workflows, and user-owned skill files are preserved. Install adds missing config options, updates its own workflow skill, and removes retired skills only when the installer manifest owns them. The package itself can then be moved or archived. Keep a copy if you want the same `flowdex uninstall` command later.

Uninstall removes the copied backend and app override while preserving all user data and global assets. Add `--purge` to also remove only the global config, default workflows, and skills that this installer created. Pre-existing or additional global files, Flowdex runtime history, and repository-owned `.flowdex/` directories are preserved.

## Automatic upstream releases

On a GitHub fork, `Flowdex upstream release` checks the latest stable `openai/codex` release daily. A clean upstream merge produces Windows and Intel/Apple Silicon macOS packages, opens a source-sync pull request, and publishes a matching Flowdex release. A merge conflict stops the release and opens an issue for manual resolution.

Enable read/write workflow permissions and allow Actions to create pull requests in the fork's Actions settings. The workflow is disabled automatically in `openai/codex` itself.
