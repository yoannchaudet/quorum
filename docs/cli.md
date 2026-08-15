# Command Line Interface

> The CLI is a **reference driver** over the Core (see [architecture.md](architecture.md)
> and [frontend.md](frontend.md)); the Tauri UX exposes the same Core operations. It will
> be retired once the UX reaches parity.

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
│   ├── start <WORK_ITEM.md> [--base REVISION] [--target BRANCH] [--remote NAME] [--dry-run]
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
    ├── approve <WORK_ITEM> [--remote NAME --target BRANCH] [--dry-run]
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

`start --base` resolves a commit before the worktree exists; omitting it uses committed
`HEAD`. The remote defaults to `origin`. The target is inferred only from an
unambiguous branch; otherwise `--target` is required.

## Focused review commands

`plan show` prints raw Plan Markdown by default for paging, copying, or diffing.
`--metadata` includes convergence, feedback, and execution authorization; `--json`
emits a focused version 2 Plan document.

`implementation show` reports implementation rounds, reviews, artifacts, workspace state,
and delivery handoff. Its `--json` output is an independently versioned focused version 3
document.

Plan and implementation approval/rejection commands require `PlanReview` and
`WorkReview`, respectively. Wrong-state actions fail rather than dispatching based on
the current state. `implementation approve --remote --target` only fills missing
settings on migrated legacy work items. Successful delivery prints the PR URL; Quorum
never merges or enables auto-merge.

## Global options

| Option | Behavior |
|---|---|
| `--config PATH` | Override `~/.quorum/config.yaml`. |
| `--context PATH` | Resolve repository scope from this folder instead of cwd. |
| `--quiet` | Suppress live autonomous progress on stderr. |
