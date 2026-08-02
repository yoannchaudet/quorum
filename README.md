# Quorum

<p align="center">
  <img src="src-tauri/icons/icon.svg" alt="Quorum logo" width="180">
</p>

Quorum is a lightweight macOS harness for semi-autonomous software work. Built
on the Copilot CLI, it turns a Markdown request or GitHub issue into a plan,
implementation, reviewed pull request, and guarded merge.

## Status

Quorum is being designed as a minimal Tauri 2 application. The initial roadmap
is open; no usable app has shipped yet.

## Workflow

A work item can start as text typed in Quorum, a local Markdown file, or a
GitHub issue:

```text
intake -> multi-agent plan -> questions -> optional plan review
       -> build -> adversarial review -> pull request
       -> Copilot review/fix loop -> merge -> notification
```

Reviewed plans can be saved before implementation and queued for unattended
work. Jobs run sequentially by default, while independent jobs can run in
parallel in isolated Git worktrees.

## Local by design

Quorum manages multiple local repositories but keeps its own durable SQLite
state outside them. Requirements, plans, answers, run history, and queue state
are not committed to target repositories. The app uses the user's existing
Copilot and GitHub CLI sessions.

Automation stops instead of bypassing repository protections: required checks
must pass, Copilot comments must be resolved, and remediation is limited to
three rounds. Blocked work remains inspectable and resumable.

## MVP architecture

- Tauri 2 macOS app with a Svelte/TypeScript interface
- Rust orchestration and typed IPC
- SQLite state in the application data directory
- Copilot CLI for planning, building, and review
- GitHub CLI for issue, pull request, check, review, and merge operations
- Native macOS completion and blocked-work notifications

## Roadmap

| Milestone | Outcome |
| --- | --- |
| [M1: App and local state](https://github.com/yoannchaudet/quorum/issues/1) | Launchable Tauri app, repository registry, SQLite state, and Markdown rendering |
| [M2: Planning and pre-planning](https://github.com/yoannchaudet/quorum/issues/2) | Three intake paths, multi-agent questions and plans, optional approval, and enqueueing |
| [M3: Build and adversarial review](https://github.com/yoannchaudet/quorum/issues/3) | Resumable Copilot execution and review in isolated worktrees |
| [M4: Pull request through merge](https://github.com/yoannchaudet/quorum/issues/4) | Checks, bounded Copilot remediation, guarded merge, and notifications |
| [M5: Durable work queues](https://github.com/yoannchaudet/quorum/issues/5) | Sequential scheduling, explicit parallel work, controls, and restart recovery |

## Development

Quorum M1 supports macOS 14 Sonoma or later. Install a current Node.js LTS,
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
