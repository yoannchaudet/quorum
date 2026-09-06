# Independent planning and focused follow-ups

The first candidates are independent. Their value is different reasoning, not a vote
or a guarantee that repeated rounds will reach the right answer.

## Launching planners

Launch both planners in one parallel batch using `task`, `agent_type: explore`,
`mode: background`, and explicit model overrides. Defaults are `claude-opus-5` and
`gpt-6-astra`; select available alternatives from different vendors if needed.
Keep the agents alive for focused follow-ups through `write_agent`.

## Candidate prompt

> You are a Planner. Produce a candidate implementation plan.
>
> Rules:
> - Work independently from other planners. Read the repository, work item, and human
>   answers; do not read other candidates or an existing merged draft.
> - Read-only. Do not modify files.
> - Specify what to build, in what order, and why.
> - Ground decisions in actual files and symbols.
> - Prefer the simplest approach that satisfies the work item.
> - Identify actionable risks and uncertainties that could change the approach.
> - Describe verification using existing repository checks; do not assume commands or
>   tools exist or propose build-system changes just to satisfy Quorum.
>
> Work item: `{work_item}`
> Human answers: `{answers}`
> Human feedback, if replanning: `{feedback}`
>
> Return a markdown plan with:
> - `## Summary`
> - `## Steps` - ordered and verifiable
> - `## Risks & assumptions`
> - `## Verification` - checks and evidence for the requested behavior, including gaps

Save candidates as `quorum/plans/round-{n}/{slot}.md` under the session artifacts
directory before merging. Use a new round directory for fresh candidates after a
materially changed work item.

## Merging

Merge the candidates yourself:

- Reconcile overlap and choose the simplest supported approach.
- Weight evidence over headcount. One concrete argument can outweigh two preferences.
- Do not invent scope beyond the work item.
- Retain actionable risks, not every speculative concern. Explain dismissals of material
  concerns so dissent does not disappear silently.
- Distinguish decisions you can resolve from those requiring more evidence or human input.

Write `quorum/plans/plan.md` with:

- `## Summary`
- `## Steps`
- `## Risks & assumptions`
- `## Verification`
- `## Decisions & disagreements` - alternatives, chosen reasoning, dismissed material
  concerns, and any unresolved dissent; `NONE` if there is none

If no material decision needs more evidence, proceed directly to human approval.
Agreement among planners is not an additional gate.

## Focused follow-ups

Send only the relevant decision and evidence to the live planners using `write_agent`.
They may now see the competing reasoning; independence applies to the initial candidates,
not to this discussion. Do not ask them to refine toward or agree with your draft.

> The open decision is `{decision}`.
> Competing approaches and evidence: `{alternatives}`.
> Answer this specific question: `{question}`.
> Check the repository where useful. Remain read-only. State which approach you support,
> why, what evidence would change your recommendation, and any remaining material risk.
> Do not rewrite the whole plan or agree merely to end the discussion.

A third available model from a different vendor may answer this same focused prompt
when the decision needs another opinion. Do not add a third planner routinely.
Save responses as `quorum/plans/round-{n}/followup-{m}-{slot}.md` and update the draft.

Cap follow-ups at two rounds after each fresh candidate batch. At the cap, show the
remaining alternatives to the human rather than spawning more agents. If a blocking
question needs human input, ask it directly and update the affected parts. Obtain fresh
independent candidates only if the answer changes the goal or core approach.

## Approval

Present the draft and any unresolved dissent with `ask_user`. On rejection, address the
feedback in affected sections; do not restart unaffected planning. Material changes to
the goal or approach need fresh candidates.

Invalidate `quorum/plans/approved-plan.md` before any draft revision. Recreate it from
the exact current draft only after explicit human approval. A missing approved file or
one older than the draft cannot authorize a build.
