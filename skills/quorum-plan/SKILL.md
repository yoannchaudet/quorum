---
name: quorum-plan
description: Produce a converged implementation plan from a work item, and nothing else. Runs a small fleet of planner models in isolation, merges their candidate plans into one, and gates on the human. Writes no code. Use when asked to plan or spec a change, for a second opinion on an approach, or when the user says "quorum plan". For plan *and* build, use the quorum skill; to implement an existing plan, use quorum-build.
user-invocable: true
---

# Quorum — plan

Planning is the specification. Get it right and the work goes well. So planning is
delegated to a **quorum** of independent models and merged into one plan.

This skill stops at an approved plan. It never writes code. Hand the result to
`quorum-build` when you want it implemented.

You are the **Coordinator**. You own the state machine below.

```
Intake ──> Fleet planning ──> Converge ──┐
   ^            ^                        │ ITERATE
   │            └────────────────────────┘
   │                    │ CONVERGED
   │                    v
   └── answers ──  Plan gate (human) ──┐ reject
                        │ approve      │
                        v              └──> Fleet planning
                    Approved plan
```

## Profile

Read the caller's profile. Default to **light** unless a caller (normally the `quorum`
skill) explicitly says `full`.

| | `light` (default) | `full` |
|---|---|---|
| Planner roster | 2 models | 3 models |
| Intake | You read the work item and ask only blocking questions yourself | One read-only intake sub-agent per planner, in parallel |
| Convergence rounds | 1 | 3 |
| Plan gate | Required | Required |

`light` is the small-task path: two opinions, one merge, one approval. Escalate to `full`
mid-run if the work turns out to be larger or more contentious than it looked, and say so
when you do.

## Before anything

Create a working directory for this run under the session artifacts dir: `quorum/` with
`plans/` inside it. Every candidate plan and the merged plan is written there as a file,
never kept only in the transcript. The merged plan lives at `quorum/plans/plan.md` and is
a **draft** until the human approves it; approval promotes it to
`quorum/plans/approved-plan.md`, which is what `quorum-build` reads.

You do **not** need the Makefile contract to plan, and you do not bootstrap it —
`quorum-build` owns that. The plan's `## Verification` section still speaks in terms of
`make verify` and `make verify-full`; see
[../quorum/references/makefile.md](../quorum/references/makefile.md) for what those mean.

## Phase 0 — Intake

The work item is a GitHub issue (`gh issue view <n>`), a markdown file, or a prompt.
Resolve it to text and save it as `quorum/work-item.md`.

In `light`, read the work item and the repository yourself, and ask the human only the
questions whose answers would change the plan.

In `full`, run one intake sub-agent per planner model, read-only, in a single parallel
batch. Each returns either `NONE` or the **minimum** numbered questions. Dedupe and merge
overlapping questions.

Either way, ask the human **one question at a time** with `ask_user`, offering multiple
choice whenever the options are predictable, and record answers in `quorum/answers.md`.

Prefer zero questions. Do not ask about nice-to-haves.

## Phase 1 — Fleet planning

**Invalidate any stale approval first.** If `quorum/plans/approved-plan.md` exists from an
earlier run or an earlier round, delete it before you plan. An approval only ever applies
to the draft the human actually saw; the moment you start producing a new one, the old
approval is void. This is what keeps a re-plan — after a rejection, new answers, or scope
drift bounced back from `quorum-build` — from leaving a stale approval lying around for a
later session to build against.

Launch the planner roster in **one parallel batch** of background sub-agents, each with an
explicit `model` override. Full protocol and prompts in
[references/planning.md](references/planning.md).

Default roster (override on request):

| Slot | Model | Role | Profile |
|------|-------|------|---------|
| `planner-a` | `claude-opus-5` | Primary generalist | both |
| `planner-b` | `gpt-5.6-sol` | Independent, different vendor | both |
| `planner-c` | `gemini-3.1-pro-preview` | Third opinion / tie-breaker | `full` only |

Each planner works in **isolation**: it sees the work item and human answers and nothing
else. It never sees another planner's output. This is the whole point — do not summarize
one planner's plan into another's prompt.

Two planners is the floor. Below that there is no quorum, and you should be using neither
this skill nor `quorum`.

## Phase 2 — Converge

Merge the candidates yourself into `quorum/plans/plan.md`, then emit `CONVERGED` or
`ITERATE`. On `ITERATE`, send the merged plan back to the **same live planners** with
`write_agent` so they keep their context, and merge again. Cap at the profile's round
limit — **1** in `light`, **3** in `full` — then take the current merged plan and tell the
human which disagreements never resolved. Criteria and merge rules in
[references/planning.md](references/planning.md).

## Phase 3 — Plan gate (human)

Show the merged plan and ask for approval. On rejection, feed the feedback into a new
planning round — every requested change must be addressed. This gate is not optional.

`quorum/plans/plan.md` is a **working draft** at this point, not something anyone should
build against. Only on approval do you copy it to `quorum/plans/approved-plan.md`. That
file is the approval record, and it is what `quorum-build` looks for. Never write it
before the human says yes, and — because you deleted any stale copy when the round started
— it always describes the draft the human actually approved, never an earlier one.

## Delivering the plan

On approval, stop. Report where the approved plan lives
(`quorum/plans/approved-plan.md`), summarize it, and offer the obvious next step: run
`quorum-build` against it.

If the human asks you to implement it now, invoke `quorum-build` rather than implementing
inline — it owns the verification and review loop.

## Rules that hold across every phase

- Planners are **read-only**. Only you write files.
- Every candidate and merged plan is a file under `quorum/`, so a crashed or resumed
  session can pick up where it left off.
- Never write production code in this skill. If a plan step is only provable by
  experiment, say so in `## Risks & assumptions` instead of running the experiment.
