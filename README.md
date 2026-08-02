# Quorum

Quorum is a lightweight macOS harness for semi-autonomous software work. Built
on the Copilot CLI, it currently turns a request into a durable, reviewed plan
that can be queued for later implementation.

## Status

M2 planning and pre-planning are implemented in the Tauri 2 macOS app. Quorum
accepts inline Markdown, a local Markdown file, or a GitHub issue. M2 stops
after planning and enqueueing: it does not modify registered repositories or
execute implementation work.

## Workflow

A work item follows this planning-only workflow:

```text
inline Markdown | local Markdown file | GitHub issue
       -> independent planners -> questions -> synthesis
       -> review and edit -> optional approval -> enqueue for later
```

Each planner and the synthesizer has its own UUID-backed, unique named Copilot
session. Planner outputs remain independent until synthesis. Questions can be
answered in the app or handed off to the exact agent session in a configurable
terminal.

Terminal handoff defaults to Ghostty and configurable launch arguments using
the required `{terminalApplication}`, `{repositoryPath}`, and `{sessionName}`
placeholders. Quorum reconciles automatically when terminal completion is
observable and provides manual resume/reconciliation otherwise.

The synthesized plan can be reviewed, edited, and saved as a new revision.
Plan approval is configurable per work item; when required, approval gates
enqueueing. Enqueueing only persists intent for later implementation and does
not start implementation.

## Local by design

Quorum manages multiple local repositories but keeps its durable SQLite state
outside them. Intake, agent sessions, questions and answers, plan revisions,
terminal handoffs, run history, approvals, and queue intent survive app
restarts without being committed to target repositories. The app uses the
user's existing Copilot and GitHub CLI sessions.

## M2 architecture

- Tauri 2 macOS app with a Svelte/TypeScript interface
- Rust orchestration and typed IPC
- SQLite state in the application data directory
- Copilot CLI for independent planning and synthesis sessions
- GitHub CLI for GitHub issue intake
- Configurable terminal handoff for interactive Copilot sessions

## Roadmap

| Milestone | Outcome |
| --- | --- |
| [M1: App and local state](https://github.com/yoannchaudet/quorum/issues/1) | Launchable Tauri app, repository registry, SQLite state, and Markdown rendering |
| [M2: Planning and pre-planning](https://github.com/yoannchaudet/quorum/issues/2) | Three intake paths, multi-agent questions and plans, optional approval, and enqueueing |
| [M3: Build and adversarial review](https://github.com/yoannchaudet/quorum/issues/3) | Resumable Copilot execution and review in isolated worktrees |
| [M4: Pull request through merge](https://github.com/yoannchaudet/quorum/issues/4) | Checks, bounded Copilot remediation, guarded merge, and notifications |
| [M5: Durable work queues](https://github.com/yoannchaudet/quorum/issues/5) | Sequential scheduling, explicit parallel work, controls, and restart recovery |

## Development

Quorum supports macOS 14 Sonoma or later. Install a current Node.js LTS,
the Rust stable toolchain (including `rustfmt` and `clippy`), Xcode Command Line
Tools, and Git. The app validates registered folders with your local `git`
executable.

```bash
npm ci
npm run tauri dev
```

Run the locked checks before contributing:

```bash
npm run check
npm run test:unit
npm run build
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
npm run tauri build -- --no-bundle
```

`make check`, `make test`, `make rust-check`, and `make tauri-build` provide
the corresponding shortcuts. Quorum stores its authoritative SQLite database
in the operating system application-data directory; it never writes metadata
into a registered repository.
