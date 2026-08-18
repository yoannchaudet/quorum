---
name: quorum
description: Turn a work item into a converged plan and a reviewed pull request, end to end. Runs a fleet of planner models in isolation, merges their candidate plans into one, gates on the human, then implements against an adversarial reviewer with fast/slow verification loops. Use when the user wants both a plan and the implementation for a non-trivial change, or says "quorum" with no further qualification. For planning only — a spec, a fleet-planned or multi-model plan, a second opinion, no code — use quorum-plan. To implement an existing plan, use quorum-build.
user-invocable: true
---

# Quorum

Planning is the specification. Get it right and the work goes well. So planning is
delegated to a **quorum** of independent models and merged into one plan; implementation
is then driven by a single implementer against an **adversarial reviewer** of a different
model. Humans stay in the loop at intake and plan approval.

This skill is the **end-to-end pipeline**. It owns very little itself: it runs the two
halves back to back at their heaviest settings and carries the artifact between them.

```
quorum-plan (profile: full)
    Intake ──> Fleet planning ──> Converge ──┐
       ^            ^                        │ ITERATE
       │            └────────────────────────┘
       │                    │ CONVERGED
       │                    v
       └── answers ──  Plan gate (human) ──┐ reject
                            │ approve      │
                            │              └──> Fleet planning
                            v
                  quorum/plans/approved-plan.md   <── the handoff
                            │
                            v
quorum-build (profile: full)
                    Implement ──> make verify ──> Adversarial review
                        ^                              │
                        └──────── REJECT ──────────────┤
                                                       │ ACCEPT
                                                       v
                                                make verify-full
                                                       │ pass
                                                       v
                                                    Deliver
```

## Which skill do I want?

| You want | Use |
|---|---|
| A plan *and* the pull request, for non-trivial work | `quorum` (this one) |
| Just a plan — a spec, a second opinion, no code | `quorum-plan` |
| To implement something already specified, with real review | `quorum-build` |

If the work is small, prefer the halves. `quorum` deliberately spends more: a three-model
planner fleet, fleet intake, up to three convergence rounds, and up to five review rounds.
That is worth it for a change you would otherwise design badly, and pure overhead for a
one-file fix. If a run turns out to be smaller than it looked, say so and drop to the
light path rather than grinding through the full machine.

## Before anything

Run the read-only contract check from
[references/makefile.md](references/makefile.md) so a missing `make verify` /
`make verify-full` surfaces now rather than after a planning round. Report what you find,
but do **not** bootstrap here — `quorum-build` owns the contract and will establish it in
Phase B, where profile `full` requires it.

Create a working directory for this run under the session artifacts dir: `quorum/` with
`plans/` inside it. Both halves read and write there, and it is what lets a crashed or
resumed session pick up where it left off.

## Phase A — Plan

Invoke the **`quorum-plan`** skill with **profile `full`**. If skill invocation is not
available, follow [../quorum-plan/SKILL.md](../quorum-plan/SKILL.md) directly, in full.

At `full` it runs fleet intake across the planner roster, three planner models in
isolation, up to three convergence rounds, and the human plan gate. Do not shortcut any of
those on its behalf, and do not merge plans yourself outside of it.

It ends with a human-approved plan at `quorum/plans/approved-plan.md`. That file is the
handoff, and its existence — not older than the `quorum/plans/plan.md` draft it was copied
from — is the proof the gate was passed. Do not paraphrase it into the next phase; pass
the path. If it is missing or older than the draft, the current plan was never approved
and Phase B must not start.

## Phase B — Build

Invoke the **`quorum-build`** skill with **profile `full`**, pointing it at
`quorum/plans/approved-plan.md`. If skill invocation is not available, follow
[../quorum-build/SKILL.md](../quorum-build/SKILL.md) directly, in full.

At `full` it establishes the Makefile contract, runs up to five implement/review rounds
against an adversarial reviewer of a different model, runs `make verify-full` after the
reviewer accepts, and opens a pull request. **Never merge it.** A human owns the merge.

## Handling escapes

- **Plan rejected at the gate** — that loop lives inside `quorum-plan`. Let it run its
  rounds; do not start building against a rejected plan.
- **Reviewer rejects on something outside the plan** — that is scope drift, not a fix.
  Take it back to `quorum-plan` with the finding as feedback. It will void the existing
  approval when it reopens planning, so Phase B restarts only once the revised plan has
  been approved in its own right.
- **Either half hits its stop condition** — surface the state, the open findings, and the
  question to the human. Do not silently restart the phase.

## Rules that hold across every phase

- Planners and the reviewer are **read-only**. Only the coordinator/implementer writes
  files.
- Reviewer and implementer must be **different models**. If the roster would collide,
  pick another reviewer model.
- Every plan, review, and verdict is a file under `quorum/`, never only in the transcript.
- `make verify` before every review; `make verify-full` only after an `ACCEPT`.
- Quorum opens a pull request. It never merges one.
