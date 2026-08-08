# Agents

Four roles drive a WI. All are AI models invoked via the `copilot` CLI. The CO is the
only stateful orchestrator; PL/IM/RV are stateless workers given explicit inputs.

## Coordinator (CO)

- Owns the [state machine](state-machine.md) for one WI.
- Runs PLs, merges their output, drives convergence.
- Runs the adversarial IM↔RV loop.
- Pulls humans in (HI) via [Sessions](sessions.md) when blocked.
- Persists after every step so it can resume after a crash (see [persistence](persistence.md)).
- Emits typed activity before and after long-running work so frontends can show live
  progress without parsing agent output (see [observability](observability.md)).

## Planners (PL) — the quorum

- Each PL produces a **candidate plan** for the WI **in isolation** (*idea isolation*): it
  sees the WI and HI answers, but never another PL's output. This keeps ideas independent.
  (Distinct from *execution isolation* — see below.)
- PLs may raise **follow-up questions**. If any do, the CO enters `IntakeReview` (HI),
  collects answers, and re-runs the PLs.

### Default roster

A fixed default set of PLs ships in the specs, **overridable** in [config](config.md)
(`planners:`). Default roster:

| Slot | Purpose |
|------|---------|
| `planner-a` | Primary generalist model. |
| `planner-b` | Second independent model, different vendor/family. |
| `planner-c` | Third independent model for tie-breaking / breadth. |

Exact model IDs are set in config; the docs fix the *number and roles*, not the vendors.

## Convergence loop

1. `Planning`: all PLs produce candidate plans independently.
2. `Converging`: the CO merges candidates into one Plan.
3. Convergence criteria — the CO accepts the Plan when **both**:
   - no PL raised a new open question, and
   - the merged Plan is **stable**: re-running PLs against the current merged Plan yields
     no material changes (a diff below the configured threshold), OR the configured
     max iterations is reached.
4. If not converged, loop back to `Planning` feeding the merged Plan as context.

## Implementer (IM) & Reviewer (RV)

- IM produces the implementation from the accepted Plan.
- RV is a **different** model that adversarially reviews IM's output.
- **Adversarial loop**: `Implementing` → `Reviewing` → `Implementing` until RV accepts
  or the CO forces `WorkReview` because two rejected rounds have the same Git tree or
  the maximum iteration count is reached.
- The CO, not IM, stages and commits each changed round. Empty rounds retain the
  existing commit while recording its tree SHA.

## Human Intervention (HI)

The CO pulls humans in at exactly three points, each a blocked state:

| State | What the human does |
|-------|---------------------|
| `IntakeReview` | Answers PL follow-up questions. |
| `PlanReview` (optional) | Approves or rejects the converged Plan. |
| `WorkReview` (optional) | Approves or rejects the accepted work. |

All HI happens through a resumable [Session](sessions.md). All prompts the CO gives any
agent live as reviewable markdown files (see [prompts](prompts.md)).

## Execution isolation

Every agent (PL, IM, RV) runs inside a Local Sandbox (LS), non-interactively, with a
per-role filesystem profile. This is separate from the *idea isolation* above.

| Role | Filesystem |
|------|-----------|
| PL | read-only |
| RV | read-only |
| IM | read/write, confined to its WI workspace |

Full details — invocation flags, deny-list, network policy — are in
[isolation](isolation.md).
