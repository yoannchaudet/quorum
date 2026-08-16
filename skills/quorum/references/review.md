# Adversarial review loop

The implementer and the reviewer are deliberately different models. A model reviewing its
own work rationalizes it; a different model, told to be adversarial, does not. The
reviewer's job is to find what is wrong, not to be agreeable.

## Round structure

```
implement / fix
      │
      v
make verify ──fail──> fix (do not review)
      │ pass
      v
rubber-duck review
      │
      ├── REJECT ──> fix ──> make verify ──> review
      │
      └── ACCEPT
             │
             v
      make verify-full ──fail──> findings ──> fix ──> make verify ──> review
             │ pass
             v
          deliver
```

Two rules make this work:

- **`make verify` gates every review.** The reviewer must never spend a round reporting
  something a unit test already catches. A failing fast loop is your problem, not theirs.
- **`make verify-full` runs once, after `ACCEPT`.** It is slow, so it is the last gate,
  not the inner loop. Its failures re-enter the fast loop like any other finding.

## Launching the reviewer

Use `agent_type: rubber-duck`, `mode: sync`, with an explicit `model` override that
**differs from your own model**. If your model is the natural reviewer choice, pick
another strong model from a different vendor. Suggested pairing: implement with
`claude-opus-5`, review with `gpt-5.6-sol`, or the reverse.

The reviewer is stateless. Give it the full context every round: work item, approved
plan, what changed this round, the diff, the `make verify` output, and its own previous
findings so it can check whether they were actually addressed.

## Reviewer prompt

> You are the **Reviewer**, and you are adversarial by design. You are a different model
> from the Implementer. Assume the implementation is wrong until the evidence says
> otherwise, and go looking for that evidence.
>
> Rules:
> - Hunt for correctness bugs, missed plan steps, security issues, unhandled edge cases,
>   broken error paths, and silent behavior changes.
> - Read the actual code, not just the summary. The summary may be wrong or incomplete.
> - Judge against the **plan and the work item** — not personal style preferences. Do not
>   report formatting, naming, or taste. The fast loop already covers lint and format.
> - Verify that each of your previous findings was genuinely fixed, not worked around or
>   suppressed.
> - Check that tests actually exercise the new behavior. A passing suite that never calls
>   the new code is a finding.
> - Read-only. Do not modify files.
> - Reject if anything material is wrong or missing. Accept only when the work is sound.
>   Do not accept out of politeness or fatigue.
>
> Inputs:
> - Work item: `{work_item}`
> - Approved plan: `{plan}`
> - Changes this round (summary + diff): `{implementation}`
> - `make verify` output: `{verify_output}`
> - Your previous findings (may be empty): `{previous_findings}`
>
> Output — return a markdown document with exactly:
> - `## Verdict` — exactly `ACCEPT` or `REJECT` on its own line.
> - `## Findings` — for `REJECT`, a numbered list of concrete, actionable issues, each
>   fixable by the Implementer and each naming the file and the specific problem. For
>   `ACCEPT`, `NONE` or brief non-blocking notes.
>
> Nothing outside these sections.

Write each verdict to `quorum/reviews/round-{n}.md`.

## Implementer rules

- Follow the approved plan. If a step is wrong or infeasible, do the smallest correct
  thing and record the deviation.
- Keep changes scoped to the plan. Unrelated cleanup is out of scope and gives the
  reviewer noise to reject on.
- Honor repository conventions and any `AGENTS.md` / instruction files you find.
- Address **every** finding, or explain in the next round why a finding is wrong. Do not
  silently ignore one — the reviewer checks.
- Never suppress a test, widen a lint exclusion, or weaken an assertion to make the loop
  go green. That is an automatic reject and it is a lie to the human.

## Stop conditions

The loop must terminate. Escalate to the human when any of these hit:

| Condition | Why | Action |
|-----------|-----|--------|
| 5 review rounds | Diminishing returns | Present the state, the open findings, and ask how to proceed |
| Two consecutive rejected rounds with an identical git tree | You are not actually changing anything; the loop is stuck | Stop, explain what you could not fix |
| Reviewer rejects on something outside the approved plan | Scope drift | Take it back to the plan gate, not the fix loop |
| `make verify-full` fails for the same reason twice | The slow gate found something the plan did not anticipate | Escalate with the failure |

Record the git tree SHA (`git write-tree` or `git rev-parse HEAD^{tree}`) after each round
so the identical-tree condition can actually be detected.

## Delivering

Once the reviewer accepts and `make verify-full` passes:

1. Commit with a message naming the work item and summarizing the change.
2. Push the branch.
3. Open a pull request whose body links the work item and includes the converged plan and
   the reviewer's final verdict.
4. Stop. Never merge — the human owns that.
