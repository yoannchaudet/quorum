# Persistence & Crash Resilience

The Core runs mostly unattended, so it must survive process crashes at any point.
All structured state lives in one global SQLite database. Every query used by the
Coordinator is scoped to a stable internal work-item ID.

## On-disk layout

```
~/.quorum/
  quorum.db                     # structured state for every WI
  state/<work-item-id>/
    assets/                     # binary image files referenced by the WI
    implementation/             # linked Git worktree
```

The internal ID is a UUID, independent of the user-facing WI slug. Repository ownership
is associated in the database; it does not determine the filesystem path.

## Schema

| Table | Holds |
|-------|-------|
| `repositories` | Stable ID, canonical root, and active registration status. |
| `work_items` | Stable ID, repository, slug, normalized markdown, and source metadata. |
| `states` | Current state per WI. |
| `transitions` | Append-only transition history per WI. |
| `candidates` | PL candidate plans by WI, planner, and iteration. |
| `plans` | Converged Plan and metrics per WI. |
| `implementations` | IM summary by WI and adversarial iteration. |
| `implementation_rounds` | Start/result commits, tree SHA, and recovery status per IM round. |
| `intake` | Current planner questions per WI. |
| `reviews` | RV feedback and verdict by WI and iteration. |
| `sessions` | HI session records by WI and blocked state. |
| `events` | Append-only audit events per WI. |
| `worktrees` | Pinned base commit, branch, path, and setup status per WI. |

Every WI-owned row has a foreign key to `work_items` with cascading deletion. WI slugs
are unique per repository. Composite keys include the WI ID, preventing planners,
iterations, sessions, or reviews from colliding across WIs.

## Connection policy

- **WAL mode** permits readers while another process commits.
- **Foreign keys** are enabled on every connection.
- **Busy timeout** bounds lock contention instead of failing immediately.
- **Short transactions** never span agent or external-command execution.

## Guarantees

- **Atomic transitions**: state, transition history, and authorizing events commit in one
  transaction.
- **Scoped access**: the Coordinator receives a WI-scoped store rather than unrestricted
  catalog access.
- **Single source of truth**: the `states` row for a WI is authoritative.
- **Idempotent outputs**: deterministic composite keys let a retry replace the same
  planner, implementation, or review iteration.
- **Recoverable worktree setup**: creation intent is persisted before Git is changed;
  restart reconciles only matching branches and paths.
- **Recoverable implementation rounds**: `running` records are created before IM,
  summaries and `agent_complete` are persisted atomically, and `committed` records hold
  the resulting commit and tree SHA.
- **Attributable Git history**: the CO stages with `git add -A` and creates marked
  commits using a fixed Quorum identity. Empty rounds record the unchanged tree without
  creating an empty commit.
- **Confined agent writes**: Local Sandbox restricts file writes to the linked worktree
  (see [isolation](isolation.md)).

## Resume after crash

1. Resolve and validate the registered repository context.
2. Find the WI by repository and user-facing ID in the global catalog.
3. Read its scoped `states` row.
4. Verify the outputs required by that state.
5. Advance when complete; otherwise re-run the idempotent step.
6. For blocked states, re-derive and surface the persisted Session name.

For an implementation round, restart behavior depends on its status:

- `running`: require the original HEAD and rerun IM over any partial file edits.
- `agent_complete`: stage/finalize the edits, or adopt an already-created commit whose
  parent and Quorum markers match the round.
- `committed`: skip IM and continue from the persisted result.

An unrelated commit or unexpected HEAD is never reset or overwritten; the WI fails with
the checkout left intact for inspection.

## Failure

A step retries up to `limits.step_retries` before the CO moves the WI to `Failed`.
`Failed` is terminal, but its structured history and filesystem workspace remain
available for inspection.
