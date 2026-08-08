# Work Items

A work item is a **local markdown file**. It is the single input Quorum processes.
Every work item is scoped to a registered Git repository (see
[repositories](repositories.md)); its user-facing ID is unique within that repository.

## Sources

| Source | How |
|--------|-----|
| Local | Point the CLI at an existing `.md` file. |
| GitHub | The CLI pulls a GitHub issue and writes it to a local `.md` file (body + metadata). The repo defaults to the one for the current working directory; it can also be passed on the CLI. From then on it is treated as a local work item. |

Pulling from GitHub is a one-time import: Quorum operates on the local copy so it can run unattended and offline.

## Images

Images are first-class — they will be used extensively.

- Markdown embeds images GitHub-style: `![alt](path-or-url)`.
- **Local images**: relative paths are resolved against the work-item file's directory.
- **Remote images** (e.g. GitHub-hosted attachments): downloaded during `Intake` and
  rewritten to local paths so the work item is self-contained and reproducible.
- Downloaded assets are stored alongside the work item's on-disk state.

## On-disk shape

```
~/.quorum/
  quorum.db           # structured state for every work item
  state/<work-item-id>/
    assets/           # downloaded/embedded images
    implementation/   # linked Git worktree
```

The work item's normalized markdown and stable internal ID are stored in the global database.
Only binary images and implementation files live under the work item's UUID-keyed state
directory.

`implementation/` is a linked Git worktree on a dedicated Quorum branch. Its base is the
context repository's committed `HEAD` when the work item first runs; uncommitted changes
in the user's checkout are deliberately excluded. Planners and the Reviewer use the
worktree read-only, while the Implementer uses it read/write.

## Validation (during Intake)

- File exists and is valid UTF-8 markdown.
- All embedded image references resolve (local or successfully downloaded).
- On failure the Coordinator moves to `Failed` after retries.

See [state-machine](state-machine.md) for where Intake sits.
