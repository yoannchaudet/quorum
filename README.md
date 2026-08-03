# Quorum

<p align="center">
  <img src="src-tauri/icons/icon.svg" alt="Quorum logo" width="180">
</p>

Quorum is a lightweight macOS harness for semi-autonomous software work. Built
on the Copilot CLI, it turns a request into a durable, reviewed plan and can
execute one queued plan through verification and adversarial review.

## Status

M3 execution is implemented in the Tauri 2 macOS app. Quorum accepts inline
Markdown, a local Markdown file, or a GitHub issue, plans the work, and starts
one queued plan only through an explicit action. Each run uses a dedicated
managed Git branch and worktree outside the registered checkout.

## Workflow

A work item follows this workflow:

```text
inline Markdown | local Markdown file | GitHub issue
       -> independent planners -> questions -> synthesis
       -> review and edit -> optional approval -> enqueue
       -> isolated builder -> repository verification -> adversarial review
       -> bounded remediation and focused re-review -> ready for delivery
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
enqueueing. Enqueueing does not auto-start work: the queued plan exposes a
separate execution action.

Execution captures the exact clean, attached source `HEAD`, creates a
`quorum/<work-item>-<run-id>` branch in an application-data worktree, and
starts a persisted named builder session. Quorum discovers verification in
this order: Makefile `check`, Makefile `test`, package.json `test`, then Cargo
`test`. Builder, verification, and reviewer processes run under a whole-process
macOS Seatbelt profile that permits writes only in the managed worktree. The
Copilot CLI's experimental `/sandbox` is not used as the security boundary
because it does not OS-sandbox built-in file edits; platforms without configured
whole-process confinement fail closed. After verification succeeds, a separately
named adversarial reviewer receives the persisted plan, acceptance intent,
complete bounded base diff, and verification evidence. Oversized or incomplete
evidence blocks delivery rather than being truncated.

Phase history, bounded command output, attempts, sessions, verification
arguments, findings, and dispositions survive restarts. Interrupted work is
blocked and resumed through a new owned attempt. Durable app/run file leases and
explicit branch/worktree ownership claims prevent concurrent recovery and stale
resume; persisted process identifiers are never trusted. Cancellation targets
only the current run's owned process group. Quorum never resets, stashes, or
cleans user work.

## Local by design

Quorum manages multiple local repositories but keeps its durable SQLite state
outside them. Intake, agent sessions, questions and answers, plan revisions,
terminal handoffs, run history, approvals, and queue intent survive app
restarts without being committed to target repositories. The app uses the
user's existing Copilot and GitHub CLI sessions.

## M3 architecture

- Tauri 2 macOS app with a Svelte/TypeScript interface
- Rust orchestration and typed IPC
- SQLite state in the application data directory
- Copilot CLI for independent planning and synthesis sessions
- Managed Git worktrees for builder and adversarial-review execution
- Persisted verification evidence, bounded logs, findings, and dispositions
- GitHub CLI for GitHub issue intake
- Configurable terminal handoff for interactive Copilot sessions

M3 does not push branches, open pull requests, remediate pull-request review,
or schedule multiple queued jobs.

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
