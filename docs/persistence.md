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
    implementation/             # IM working output
```

The internal ID is a UUID, independent of the user-facing WI slug. Repository ownership
will be associated in the database; it does not determine the filesystem path.

## Schema

| Table | Holds |
|-------|-------|
| `work_items` | Stable ID, slug, normalized markdown, and source metadata. |
| `states` | Current state per WI. |
| `transitions` | Append-only transition history per WI. |
| `candidates` | PL candidate plans by WI, planner, and iteration. |
| `plans` | Converged Plan and metrics per WI. |
| `implementations` | IM summary by WI and adversarial iteration. |
| `intake` | Current planner questions per WI. |
| `reviews` | RV feedback and verdict by WI and iteration. |
| `sessions` | HI session records by WI and blocked state. |
| `events` | Append-only audit events per WI. |

Every WI-owned row has a foreign key to `work_items` with cascading deletion. Composite
keys include the WI ID, preventing planners, iterations, sessions, or reviews from
colliding across WIs.

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
- **Confined agent writes**: Local Sandbox restricts file writes to the WI workspace
  (see [isolation](isolation.md)).

## Resume after crash

1. Find the WI by its user-facing ID in the global catalog.
2. Read its scoped `states` row.
3. Verify the outputs required by that state.
4. Advance when complete; otherwise re-run the idempotent step.
5. For blocked states, re-derive and surface the persisted Session name.

## Failure

A step retries up to `limits.step_retries` before the CO moves the WI to `Failed`.
`Failed` is terminal, but its structured history and filesystem workspace remain
available for inspection.
