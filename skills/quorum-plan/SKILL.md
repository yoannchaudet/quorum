---
name: quorum-plan
description: Produce a human-approved implementation plan from a work item, and nothing else. Two planner models work independently, a coordinator merges their candidates, and focused follow-ups resolve material disagreements. Writes no code. Use when asked to plan or spec a change, for a second opinion on an approach, or when the user says "quorum plan". For planning and implementation, use quorum; to implement an existing plan, use quorum-build.
user-invocable: true
---

# Quorum - plan

You are the coordinator. Two independent planner models supply different approaches
and risks; you merge their reasoning into a plan for human approval. Stop at the approved
plan. Never write production code in this skill.

## Phase 0 - Intake

Resolve the work item from the prompt, a file, or a GitHub issue. Save it as
`quorum/work-item.md` under the session artifacts directory. Create `quorum/plans/` there
for candidates and the merged draft.

Read the repository yourself. Ask only blocking questions whose answers change the
plan, one at a time with `ask_user`; prefer zero questions. Record answers in
`quorum/answers.md`. Do not run a separate intake fleet.

## Phase 1 - Independent planning

Before producing or revising a plan, delete any existing
`quorum/plans/approved-plan.md`. Approval applies only to the draft the human saw;
reopening planning for new answers, rejection, or scope changes invalidates it.

Launch two read-only planners in one parallel batch, with explicit model overrides:

| Slot | Default model |
|---|---|
| `planner-a` | `claude-opus-5` |
| `planner-b` | `gpt-5.6-sol` |

Use available models from different vendors if a default is unavailable. Two independent
planners are the minimum. Each sees the work item, human answers, and repository, but
not the other candidate. Preserve each candidate as a file.

See [references/planning.md](references/planning.md) for prompts and artifact paths.
Verification uses the repository's actual checks, not assumed Make targets; see
[../quorum/references/verification.md](../quorum/references/verification.md).

## Phase 2 - Merge and resolve

Merge once into `quorum/plans/plan.md`. Weight concrete reasoning over headcount.
Retain actionable risks and explain why material concerns were dismissed.

Do not run refinement rounds merely to obtain agreement. If a material disagreement
remains, ask the relevant live planners a focused question. Use a third model only when
an unresolved decision needs another independent opinion. Preserve material dissent
rather than asking agents to agree with the merged plan.

Allow at most two focused follow-up rounds after the initial merge. If a decision still
cannot be resolved, present the alternatives and evidence to the human. Do not call
reaching the cap consensus, or silently restart the loop.

## Phase 3 - Human approval

Show the merged plan, including material disagreements, and ask for explicit approval
with `ask_user`. On rejection, invalidate any approval, address the feedback, and return
to the affected planning step. Do not rerun unaffected work by default; changes to the
goal or core approach require fresh independent candidates.

Only after approval, copy the current `quorum/plans/plan.md` to
`quorum/plans/approved-plan.md`. Never write the approved file early or leave an old
approval in place while changing the draft.

Report the approved path and stop. When called by `quorum`, return the path to that
orchestrator. If the human requests implementation, invoke `quorum-build`.

## Invariants

- Planners are read-only; only you write artifacts.
- Candidates, follow-up responses, and merged plans persist under `quorum/plans/`.
- The approved copy must not predate the current draft. A missing or stale approval
  requires a new human gate.
- If a step needs an experiment to establish feasibility, record that uncertainty in
  the plan rather than writing production code.
