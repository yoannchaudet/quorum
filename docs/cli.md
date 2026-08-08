# Command Line Interface

Quorum uses noun-based command groups. Work-item slugs are scoped to the repository
selected by `--context` or the current directory.

## Command tree

```text
quorum
├── repository
│   ├── register [PATH]
│   ├── unregister [PATH]
│   └── list
├── work-item
│   ├── start <WORK_ITEM.md> [--dry-run]
│   ├── resume <SLUG> [--dry-run]
│   ├── list [--state STATE]
│   ├── show <SLUG> [--verbose] [--json]
│   └── abandon <SLUG>
├── intake
│   ├── show <SLUG>
│   └── answer <SLUG> [TEXT] [--file PATH] [--dry-run]
├── plan
│   ├── show <SLUG> [--metadata] [--json]
│   ├── approve <SLUG> [--dry-run]
│   └── reject <SLUG> [FEEDBACK] [--file PATH] [--dry-run]
└── implementation
    ├── show <SLUG> [--verbose] [--json]
    ├── approve <SLUG> [--dry-run]
    └── reject <SLUG> [FEEDBACK] [--file PATH] [--dry-run]
```

The former top-level lifecycle commands are intentionally unsupported. Quorum is
pre-1.0; one canonical vocabulary is preferable to compatibility aliases.

## Work items

`work-item start` creates a repository-scoped work item and rejects an existing slug.
It reads and persists the Markdown before preparing the linked worktree, so
`work-item resume` can recover interrupted setup without the original file path.

`work-item list` prints slug, state, state kind, and latest activity time, most recent
first. Repeat `--state` to filter the current repository.

`work-item show` renders the complete status document. `--verbose` expands stored text;
`--json` emits the versioned machine-readable status.

## Focused review commands

`plan show` prints raw Plan Markdown by default for paging, copying, or diffing.
`--metadata` includes convergence, feedback, and execution authorization; `--json`
emits a focused version 1 Plan document.

`implementation show` reports implementation rounds, reviews, artifacts, and workspace
state. Its `--json` output is an independently versioned focused document.

Plan and implementation approval/rejection commands require `PlanReview` and
`WorkReview`, respectively. Wrong-state actions fail rather than dispatching based on
the current state.

## Global options

| Option | Behavior |
|---|---|
| `--config PATH` | Override `~/.quorum/config.yaml`. |
| `--context PATH` | Resolve repository scope from this folder instead of cwd. |
| `--quiet` | Suppress live autonomous progress on stderr. |

