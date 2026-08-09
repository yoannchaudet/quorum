# Command Line Interface

Quorum uses noun-based command groups. Work-item commands resolve a full UUID or a
unique UUID prefix within the repository selected by `--context` or the current
directory.

## Command tree

```text
quorum
├── repository
│   ├── register [PATH]
│   ├── unregister [PATH]
│   └── list
├── work-item
│   ├── start <WORK_ITEM.md> [--dry-run]
│   ├── resume <WORK_ITEM> [--dry-run]
│   ├── list [--state STATE]
│   ├── show <WORK_ITEM> [--verbose] [--json]
│   └── abandon <WORK_ITEM>
├── intake
│   ├── show <WORK_ITEM>
│   └── answer <WORK_ITEM> [TEXT] [--file PATH] [--dry-run]
├── plan
│   ├── show <WORK_ITEM> [--metadata] [--json]
│   ├── approve <WORK_ITEM> [--dry-run]
│   └── reject <WORK_ITEM> [FEEDBACK] [--file PATH] [--dry-run]
└── implementation
    ├── show <WORK_ITEM> [--verbose] [--json]
    ├── approve <WORK_ITEM> [--dry-run]
    └── reject <WORK_ITEM> [FEEDBACK] [--file PATH] [--dry-run]
```

The former top-level lifecycle commands are intentionally unsupported. Quorum is
pre-1.0; one canonical vocabulary is preferable to compatibility aliases.

## Work items

`work-item start` always creates a new repository-scoped work item, even when another
item has the same filename stem. It prints the first eight UUID characters as the
default reference. Commands accept that reference, any shorter repository-unique
prefix, or the full UUID; ambiguous prefixes require more characters.

`work-item list` prints UUID reference, non-unique label, state, state kind, and latest
activity time, most recent first. Repeat `--state` to filter the current repository.

`work-item show` renders the complete status document. `--verbose` expands stored text;
`--json` emits the versioned machine-readable status.

## Focused review commands

`plan show` prints raw Plan Markdown by default for paging, copying, or diffing.
`--metadata` includes convergence, feedback, and execution authorization; `--json`
emits a focused version 2 Plan document.

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
