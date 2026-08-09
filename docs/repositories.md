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

The first run resolves and pins the requested base (default committed `HEAD`) and creates a linked worktree at
`~/.quorum/state/<work-item-id>/implementation/`. Quorum uses a deterministic branch:

```
quorum/<sanitized-work-item-label>-<short-work-item-id>
```

The original checkout and any uncommitted changes in it remain untouched. Worktrees and
branches are retained when a work item reaches `Done`, `Failed`, or `Abandoned`.

Accepted work is delivered by the Coordinator, never by an agent: it pushes this branch
to the persisted remote and creates or adopts a GitHub pull request against the persisted
target branch. Quorum never merges, enables auto-merge, deletes the branch, or closes the PR.

## Work-item identity

The work-item UUID is canonical and commands accept a repository-unique UUID prefix.
Filename-derived labels are for display and may repeat, including within one repository.

Repository IDs and work-item IDs are opaque UUIDs stored in the global database. Canonical
paths identify registration records; they are not used as filesystem directory names.
