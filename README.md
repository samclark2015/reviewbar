# Review Bar

A cross-platform menu bar / system-tray app that watches GitHub pull requests
awaiting your review, automatically builds each one in its own git worktree, and
lets you launch it. 

Built with [Tauri 2](https://tauri.app) — a Rust backend (subprocess
orchestration, git worktrees, polling) with small web windows for settings and
the live build log. Runs natively on macOS, Windows, and Linux.

## What it does

- Polls each configured repo for PRs matching a search (default
  `review-requested:@me`).
- When a PR's head changes, creates/updates a git worktree and runs your build
  commands, streaming output to a live in-app log and firing a desktop
  notification on success/failure.
- The tray menu shows an aggregate status glyph and a per-PR submenu: **Launch**,
  **Claude Code here** (opens a terminal in the worktree running `claude`),
  **Rebuild**, **Watch build log**, **Open in editor**, **Open PR on GitHub**,
  **Remove worktree**.
- PR lifecycle when it drops out of your review queue: **approved / merged /
  closed / un-requested** → the PR and its worktree are removed;
  **changes-requested / commented** → kept (status "reviewed", worktree retained).

## Requirements

- [`gh`](https://cli.github.com) (GitHub CLI), authenticated (`gh auth login`).
  It is invoked for all GitHub queries and handles auth cross-platform.
- `git` on PATH.
- For the **Claude Code here** action: the [`claude`](https://claude.com/claude-code)
  CLI on PATH, and a terminal (iTerm/Terminal on macOS; Windows Terminal/cmd;
  gnome-terminal/konsole/xterm on Linux).
- Whatever toolchain your build commands need (node/npm, cargo, etc.). Use each
  repo's **PATH prepend** and **Environment** fields to make tools resolve the
  same way they do in your terminal (this replaces the old hardcoded mise-shims
  hack — e.g. add `~/.local/share/mise/shims`).
- Linux only: a tray/appindicator implementation
  (`libayatana-appindicator3`).

## Configuration

Open **Settings…** from the tray menu. For each repository:

| Field | Meaning |
| --- | --- |
| Display name | Shown in the tray |
| GitHub (owner/repo) | The repo to query |
| Local clone path | Source for `git worktree add` (e.g. `~/Projects/positron`) |
| Worktree base dir | Where per-PR worktrees are created |
| Search query | `gh pr list --search` query (default `review-requested:@me`) |
| Build commands | One shell command per line, run in order |
| Launch command | Command to start the built app (run detached from the worktree) |
| PATH prepend | Dirs prepended to PATH for build/launch |
| Environment | `KEY=VALUE` per line |
| Shell override | e.g. `zsh -lc` (default `sh -c` / `cmd /C` on Windows) |

Global settings: poll interval, editor open command (`{path}` is substituted),
and launch-at-login.

Config is stored as `config.json`, runtime state as `state.json`, and per-PR
build logs under the app's OS-standard config/data directories.

## Development

```sh
npm install
npm run tauri dev      # run the app
cargo test --manifest-path src-tauri/Cargo.toml   # unit tests (reconcile logic)
npm run tauri build    # produce a platform bundle
```

> **Note:** `Cargo.lock` pins `time` to `0.3.51`; `0.3.52` is incompatible with
> the `cookie 0.18.1` that Tauri pulls in. Remove the pin once that resolves
> upstream.

## Packaging

`npm run tauri build` produces `.app`/`.dmg` (macOS), `.msi`/NSIS `.exe`
(Windows), and `.deb`/`.AppImage` (Linux). A GitHub Actions workflow that builds
all three is in `.github/workflows/release.yml`.
