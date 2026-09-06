# Independent review loop

A different model provides a separate perspective, not a guarantee of correctness.
The reviewer looks for concrete defects and missed requirements, not reasons to agree
or opportunities to expand scope.

## Round structure

```
Implement / fix -> Relevant checks -> Review
       ^                               |
       +------------ REJECT -----------+
                                       | ACCEPT
                                       v
                           Remaining applicable checks -> Deliver
                                       |
                                      FAIL -> Fix and review again
```

Follow [../../quorum/references/verification.md](../../quorum/references/verification.md).
Review receives current evidence and its limitations. Before delivery, run broader
applicable checks not already covered. No second suite is mandatory; reuse successful
evidence only while valid. Review acceptance does not excuse a failed check.

## Launching the reviewer

Use a read-only review agent available in the host, such as `agent_type: code-review`,
with `mode: sync` and an explicit model override different from the implementer's.
If no specialized reviewer is available, use a general-purpose agent explicitly
instructed to remain read-only. Prefer a different vendor.

Give the reviewer full context each round: work item, spec, current diff, verification
evidence, and previous findings. Do not rely on it remembering an earlier invocation.

## Reviewer prompt

> You are the Reviewer. Investigate the actual code and seek evidence of defects.
> Remain read-only; do not modify files.
>
> - Hunt for correctness bugs, missed requirements, security issues, unhandled edge
>   cases, broken error paths, and silent behavior changes.
> - Judge against the spec and work item, not personal style or naming preferences.
> - Check that previous findings were fixed rather than suppressed.
> - Check that verification exercises the requested behavior. Assess manual evidence
>   where automation is absent; distinguish not applicable, unavailable, and failed
>   checks rather than demanding nonexistent tooling.
> - Report material verification gaps. Human acceptance of a limitation does not turn
>   it into a passing check or excuse a known implementation defect.
> - Reject material defects or missing requirements. Support findings with concrete
>   evidence; do not invent findings to appear adversarial.
>
> Work item: `{work_item}`
> Spec: `{plan}`
> Changes this round and current diff: `{implementation}`
> Verification commands, results, covered state, and limitations: `{verification}`
> Previous findings and implementer responses: `{previous_findings}`
>
> Return:
> - `## Verdict` - `ACCEPT` or `REJECT`
> - `## Findings` - numbered, actionable findings with file and problem; on acceptance,
>   `NONE` or brief non-blocking notes

Save each verdict as `quorum/reviews/round-{n}.md` under the session artifacts directory.
Record the reviewed working-tree state with it, including staged, unstaged, and relevant
untracked changes. A HEAD tree alone cannot detect edits made between reviews.

## Stop conditions

| Condition | Action |
|---|---|
| Three review rounds without deliverable acceptance and verification | Present open findings and ask the human how to proceed |
| Two consecutive rejected rounds with identical working-tree contents | Stop and explain why the work is stuck |
| A finding requires work outside the spec | Take the scope decision to the human or `quorum-plan`; do not expand scope silently |
| A pre-delivery check fails for the same reason twice | Escalate with the failure evidence |

Never reset the round count silently. If planning reopens, invalidate the prior plan
approval and wait for approval of the revised draft before continuing implementation.

## Delivery

After acceptance and applicable verification, commit, push, and open a pull request.
Include the spec or a portable summary, final verdict, verification evidence, and any
unresolved limitations explicitly accepted by the human. Never merge.
