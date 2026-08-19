# Fleet planning and convergence

Planning is delegated to a quorum of independent models. Their value comes entirely from
**isolation**: plans written without knowledge of each other surface different risks,
different orderings, and different assumptions. A single model asked three times does not.

## Launching the fleet

Launch all planners in **one response** as background `task` sub-agents so they run in
parallel. Use `agent_type: explore` for read-only planning, one call per model, with an
explicit `model` override and a `name` matching the slot.

```
task(name: "planner-a", agent_type: "explore", mode: "background",
     model: "claude-opus-5", reasoning_effort: "high", prompt: <planner prompt>)
task(name: "planner-b", agent_type: "explore", mode: "background",
     model: "gpt-5.6-sol", reasoning_effort: "high", prompt: <planner prompt>)

# full profile only
task(name: "planner-c", agent_type: "explore", mode: "background",
     model: "gemini-3.1-pro-preview", reasoning_effort: "high", prompt: <planner prompt>)
```

Keep the agents alive after they answer — convergence rounds reuse them through
`write_agent`, which preserves each planner's context and keeps re-planning cheap.

Adjust the roster on request: fewer models for small work, more for contentious work,
different vendors if the user has a preference. Two planners is the floor; below that
there is no quorum.

## Intake prompt

Used in the **`full`** profile only: sent to each planner model before planning, in the
same parallel style. In `light`, the coordinator does intake itself and skips the fleet —
but the bar for asking is identical, so hold yourself to the rules below.

> You are a **Planner** at intake. Decide whether the work item below is specified well
> enough to plan against, and if not, ask the human the **minimum** questions needed.
>
> Rules:
> - Work in isolation. You have only the work item and any prior answers.
> - Read-only. Do not modify files. You may read the repository to ground your questions.
> - Ask only questions whose answers would **change the plan**. No nice-to-haves.
> - Prefer zero questions when the work item is clear enough to plan.
>
> Work item: `{work_item}`
> Prior answers (may be empty): `{answers}`
>
> Output: if the work item is clear enough, return exactly `NONE`. Otherwise return a
> markdown numbered list of questions, one per line, each briefly answerable. Nothing else.

Merge the returned questions: drop duplicates, collapse near-duplicates into the sharper
phrasing, and drop anything already answered by the work item. Then ask the human one
question per `ask_user` call.

## Planner prompt

> You are a **Planner**. Produce a **candidate plan** for the work item below.
>
> Rules:
> - Work in isolation: you have the work item, human answers, and the repository. You
>   cannot see other planners' output — do not assume or reference it.
> - Read-only. Do not modify files.
> - Plan the *specification*, not the code: what to build, in what order, and why.
> - Ground the plan in the actual repository. Cite the files and symbols you would touch.
> - Prefer clarity and correctness over cleverness. Call out assumptions and risks.
>
> Inputs:
> - Work item: `{work_item}`
> - Human answers (may be empty): `{answers}`
> - Previous merged plan (may be empty; refine toward it if present): `{previous_plan}`
> - Human rejection feedback (may be empty; address every requested change): `{feedback}`
>
> Output — return **only** a markdown plan with these sections:
> - `## Summary` — one paragraph on the goal and approach.
> - `## Steps` — an ordered list of concrete, verifiable steps.
> - `## Risks & assumptions` — bullets; state anything you had to assume.
> - `## Verification` — how each step is proven, in terms of `make verify` and
>   `make verify-full`.
>
> No commentary outside these sections.

Write each returned plan to `quorum/plans/round-{n}/{slot}.md` before merging.

## Merging

You merge the candidates yourself — do not delegate this. Rules:

- Reconcile overlaps, resolve conflicts, keep the strongest ideas.
- Prefer the approach best supported across candidates, but weight *reasoning* over
  headcount: one planner with a concrete file-level argument beats two hand-waving.
- Note material disagreements explicitly rather than silently picking a side.
- Do not invent scope beyond what the work item and candidates support.
- Carry forward every risk any planner raised, even if only one raised it.

Write the result to `quorum/plans/plan.md`:

- `## Summary`
- `## Steps` (ordered, verifiable)
- `## Risks & assumptions`
- `## Verification`
- `## Disagreements` — where candidates diverged and why you chose what you chose, or
  `NONE`.
- `## Convergence` — one line: `CONVERGED` if this plan is materially unchanged from the
  previous merged plan (or there was none and candidates agree), otherwise `ITERATE`
  followed by a short note on what still differs.

## Convergence loop

The plan is converged when **both** hold:

1. No planner raised a new open question, and
2. the merged plan is **stable** — re-running planners against it yields no material
   change — or the round cap is reached.

Material change means a step added, removed, reordered, or a risk that alters the
approach. Wording churn is not material.

On `ITERATE`, `write_agent` each live planner with the merged plan and ask it to refine
toward it, returning the same plan sections. Re-merge. The round cap comes from the
profile: **1 round** in `light`, **3 rounds** in `full`, where round 1 is the initial
fleet plan and merge. So `light` takes the first merge and goes straight to the gate; it
buys the diversity of independent planners without paying for refinement passes. At the
cap, take the current merged plan and tell the human which disagreements never resolved.

If `light` hits the cap with a material disagreement still open, say so plainly at the
plan gate and offer to escalate to `full` rather than papering over it.

If a planner raises a new question mid-convergence, go back to intake, answer it, and
restart the round.

## Plan gate

Present the merged plan to the human with `ask_user`: approve, or reject with feedback.
On rejection, start a new planning round with the feedback threaded into every planner
prompt, and treat addressing it as a hard requirement of the next merged plan.

`quorum/plans/plan.md` is a draft. On approval — and only on approval — copy it to
`quorum/plans/approved-plan.md`. That file is the approval record and the handoff to
`quorum-build`; its absence is what stops a rejected or half-finished plan from being
implemented by a later or resumed session.

The invariant is that `approved-plan.md` is never older than `plan.md`. Deleting it when a
planning round starts, and recreating it only at approval, is what maintains that. If you
ever find yourself with an `approved-plan.md` predating the current draft, the approval is
stale — go back to the gate.
