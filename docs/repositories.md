# Repository Context

Every work item belongs to one explicitly registered Git repository. Registration is an
allow-list: Quorum refuses to process work items for repositories the user has not approved.

## Context resolution

Work-item commands resolve their repository in this order:

1. Global `--context <folder>`.
2. The current working directory.

The selected folder may be anywhere inside a non-bare Git working tree. Quorum asks Git
for the top-level directory and stores its canonical path.

## CLI

| Command | Behavior |
|---------|----------|
| `quorum repository register [<path>]` | Add or reactivate the containing repository. |
| `quorum repository unregister [<path>]` | Remove it from the active allow-list. |
| `quorum repository list` | List active repository IDs and canonical roots. |

For register/unregister, the explicit path overrides `--context`, which overrides cwd.
Registration is idempotent. Re-registering a previously removed root restores the same
stable repository ID and its work-item associations.

Unregistering never deletes work items, filesystem state, branches, or worktrees.
Work-item commands
remain blocked until that repository is registered again.

## Work-item worktrees

The first run pins the repository's committed `HEAD` and creates a linked worktree at
`~/.quorum/state/<work-item-id>/implementation/`. Quorum uses a deterministic branch:

```
quorum/<sanitized-work-item-slug>-<short-work-item-id>
```

The original checkout and any uncommitted changes in it remain untouched. Worktrees and
branches are retained when a work item reaches `Done`, `Failed`, or `Abandoned`.

## Work-item identity

The user-facing work-item slug is unique within a repository, not globally. Two
repositories may therefore each contain a work item named `example`.

Repository IDs and work-item IDs are opaque UUIDs stored in the global database. Canonical
paths identify registration records; they are not used as filesystem directory names.
