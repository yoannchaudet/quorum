# Persistence & Crash Resilience

The Core runs mostly unattended, so it must survive process crashes at any point.
State lives in a **per-WI SQLite database**. Every step is **recoverable**: on restart
the CO reopens the DB and continues from the last committed transaction.

## On-disk layout (per WI)

```
<state_dir>/<wi-id>/
  quorum.db           # all structured state (SQLite, WAL mode)
  assets/             # binary image files referenced by the WI (see work-items.md)
  implementation/     # IM working output (code / files)
```

Only binary blobs (images) and the IM's file output stay on disk. Everything
structured — WI text, state, history, candidate plans, reviews, event log — lives in
`quorum.db`.

## Schema (tables)

| Table | Holds |
|-------|-------|
| `work_item` | Normalized WI markdown + metadata (source, origin repo/issue). |
| `state` | Single row: current state + updated-at (see [state-machine](state-machine.md)). |
| `transitions` | Append-only history of every state transition (from, to, reason, ts). |
| `candidates` | PL candidate plans: `(planner, iteration, text)`. |
| `plan` | The converged Plan text + convergence metrics. |
| `reviews` | RV feedback per adversarial iteration: `(iteration, text, accepted)`. |
| `sessions` | HI session records: `(state, session_name, ts)` (see [sessions](sessions.md)). |
| `events` | Append-only event log for auditing and resume. |

## Guarantees

- **Atomic steps**: each step (PL run, merge, IM, RV) writes its outputs and advances
  `state` in a **single SQLite transaction**. A crash leaves either the whole step or
  none of it — never a partial state.
- **WAL mode**: write-ahead logging survives process crashes without corruption.
- **Single source of truth**: the `state` row is authoritative and only advances inside
  the same transaction that persisted the step's outputs.
- **Idempotent steps**: each step writes to deterministic keys (e.g. `(planner, iteration)`)
  so a re-run after a crash overwrites cleanly rather than duplicating.
- **Confined agent writes**: the Local Sandbox restricts each agent's writes to the WI
  workspace, so a crashed agent never leaves state outside `<state_dir>/<wi-id>/`
  (see [isolation](isolation.md)).

## Resume after crash

1. Open `quorum.db`, read the `state` row → current state.
2. Verify the outputs that state requires exist (rows/files) and are valid.
3. If complete, advance; if partial/missing, **re-run that step** (idempotent).
4. For blocked (HI) states, re-derive the Session name from `sessions`/state and re-print
   the resume command (see [sessions](sessions.md)) — no human input is lost.

## What each state persists (before advancing)

| State | Durable output |
|-------|----------------|
| `Intake` | `work_item` row + `assets/` |
| `Planning` | `candidates` rows for every PL |
| `Converging` | `plan` row (+ convergence metrics) |
| `Implementing` | `implementation/` files |
| `Reviewing` | `reviews` row |
| HI states | `sessions` row (session name) |

## Failure

- A step retries up to `limits.step_retries` (see [config](config.md)) before the CO
  moves the WI to `Failed`. `Failed` is terminal but `quorum.db` is preserved for
  inspection and manual restart.
