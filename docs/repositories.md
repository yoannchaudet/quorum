# Repository Context

Every WI belongs to one explicitly registered Git repository. Registration is an
allow-list: Quorum refuses to process WIs for repositories the user has not approved.

## Context resolution

WI commands resolve their repository in this order:

1. Global `--context <folder>`.
2. The current working directory.

The selected folder may be anywhere inside a non-bare Git working tree. Quorum asks Git
for the top-level directory and stores its canonical path.

## CLI

| Command | Behavior |
|---------|----------|
| `quorum repo register [<path>]` | Add or reactivate the containing repository. |
| `quorum repo unregister [<path>]` | Remove it from the active allow-list. |
| `quorum repo list` | List active repository IDs and canonical roots. |

For register/unregister, the explicit path overrides `--context`, which overrides cwd.
Registration is idempotent. Re-registering a previously removed root restores the same
stable repository ID and its WI associations.

Unregistering never deletes WIs, filesystem state, branches, or worktrees. WI commands
remain blocked until that repository is registered again.

## Work-item identity

The user-facing WI slug is unique within a repository, not globally. Two repositories
may therefore each contain a WI named `example`.

Repository IDs and WI IDs are opaque UUIDs stored in the global database. Canonical
paths identify registration records; they are not used as filesystem directory names.
