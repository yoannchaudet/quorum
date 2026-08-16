---
name: quorum
description: Turn a work item into a converged plan and a reviewed pull request. Runs a fleet of planner models in isolation, merges their candidate plans into one, gates on the human, then implements against an adversarial reviewer with fast/slow verification loops. Use when asked to plan, spec, or drive a non-trivial change end to end, or when the user says "quorum", "fleet planning", or asks for a multi-model plan.
user-invocable: true
---

# Quorum

Planning is the specification. Get it right and the work goes well. So planning is
delegated to a **quorum** of independent models and merged into one plan; implementation
is then driven by a single implementer against an **adversarial reviewer** of a different
model. Humans stay in the loop at intake and plan approval.

You are the **Coordinator**. You own the state machine below. Never skip a phase, never
merge the resulting pull request.

```
Intake ──> Fleet planning ──> Converge ──┐
   ^            ^                        │ ITERATE
   │            └────────────────────────┘
   │                    │ CONVERGED
   │                    v
   └── answers ──  Plan gate (human) ──┐ reject
                        │ approve      │
                        v              └──> Fleet planning
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

## Before anything

Establish the **Makefile contract**: the repo must expose `make verify` (fast) and
`make verify-full` (slow). If it does not, bootstrap it — see
[references/makefile.md](references/makefile.md). Do this first; the review loop depends
on it.

Create a working directory for this run under the session artifacts dir:
`quorum/` with `plans/` inside it. All candidate plans and the merged plan are written
there as files, never kept only in the transcript.

## Phase 0 — Intake

The work item is a GitHub issue (`gh issue view <n>`), a markdown file, or a prompt.
Resolve it to text and save it as `quorum/work-item.md`.

Run one intake sub-agent per planner model, read-only, in a single parallel batch. Each
returns either `NONE` or the **minimum** numbered questions whose answers would change
the plan. Dedupe and merge overlapping questions, then ask the human **one at a time**
with `ask_user`, offering multiple choice whenever the options are predictable. Record
answers in `quorum/answers.md`.

Prefer zero questions. Do not ask about nice-to-haves.

## Phase 1 — Fleet planning

Launch the planner roster in **one parallel batch** of background sub-agents, each with
an explicit `model` override. Full protocol and prompt in
[references/planning.md](references/planning.md).

Default roster (override on request):

| Slot | Model | Role |
|------|-------|------|
| `planner-a` | `claude-opus-5` | Primary generalist |
| `planner-b` | `gpt-5.6-sol` | Independent, different vendor |
| `planner-c` | `gemini-3.1-pro-preview` | Third opinion / tie-breaker |

Each planner works in **isolation**: it sees the work item and human answers and nothing
else. It never sees another planner's output. This is the whole point — do not summarize
one planner's plan into another's prompt.

## Phase 2 — Converge

Merge the candidates yourself into `quorum/plans/plan.md`, then emit `CONVERGED` or
`ITERATE`. On `ITERATE`, send the merged plan back to the **same live planners** with
`write_agent` so they keep their context, and merge again. Cap at 3 rounds, then take the
current merged plan. Criteria and merge rules in
[references/planning.md](references/planning.md).

## Phase 3 — Plan gate (human)

Show the merged plan and ask for approval. On rejection, feed the feedback into a new
planning round — every requested change must be addressed. This gate is not optional.

## Phase 4 — Implement and adversarially review

You are the implementer. Follow the approved plan; if a step is wrong or infeasible, do
the smallest correct thing and record the deviation. Keep changes scoped to the plan.

Each round, in order:

1. Implement (first round) or fix the reviewer's findings (later rounds).
2. Run **`make verify`** — the fast loop. Never hand work to the reviewer while it fails.
3. Run the **adversarial reviewer**: a `rubber-duck` sub-agent pinned to a model
   different from your own, which must return `ACCEPT` or `REJECT` plus concrete
   findings.
4. `REJECT` → go to 1. `ACCEPT` → run **`make verify-full`**, the slow loop, once. If it
   fails, the failure becomes findings and you go back to 1.

Stop conditions, both of which escalate to the human rather than looping forever:
5 rounds, or two consecutive rejected rounds producing an identical git tree.

Full reviewer prompt, verdict format, and loop rules in
[references/review.md](references/review.md).

## Phase 5 — Deliver

Commit with a message describing the work item, push the branch, and open a pull request
whose body links the work item and includes the converged plan and the reviewer's final
verdict. **Never merge it.** A human owns the merge.

## Rules that hold across every phase

- Planners and the reviewer are **read-only**. Only you write files.
- Reviewer and implementer must be **different models**. If the roster would collide,
  pick another reviewer model.
- Every plan, review, and verdict is a file under `quorum/`, so a crashed or resumed
  session can pick up where it left off.
- `make verify` before every review; `make verify-full` only after an `ACCEPT`.
