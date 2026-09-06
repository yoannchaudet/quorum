# Repository-native verification

Quorum adapts to the repository's verification workflow; it does not install its own.
Discover checks from repository instructions, scripts, and CI. Use existing commands,
including Make targets when appropriate. Do not add tooling, targets, or aliases unless
the human requests them.

`quorum-build` owns verification. Before implementation, record the selected checks and
what they cover. Planners describe how to prove the change using checks the repository
actually offers; they do not assume commands exist.

## Two verification moments

- **Before review:** run the smallest relevant checks for the changed behavior, including
  applicable tests, lint, and type checks. Do not submit known failing checks to review.
- **Before delivery:** after the reviewer accepts, run any broader applicable checks not
  already covered, such as a full suite, build, or integration tests.

These are two decisions, not two mandatory suites. The same suite can cover both.
Reuse a successful result only when its covered code, dependencies, configuration, and
relevant environment are unchanged. After a fix, rerun affected checks; do not reuse
evidence invalidated by the change. Time-sensitive or external-service checks may need
a fresh run even when code is unchanged.

## Evidence and gaps

Record commands, results, coverage, and the revision or working-tree state they apply to.
Distinguish these cases:

| Status | Meaning | Action |
|---|---|---|
| Passed | An applicable check succeeded | Keep the evidence; reuse only while valid |
| Not applicable | A check does not exercise this change, or no such check exists | Explain why and identify other evidence; no waiver needed |
| Unavailable | An applicable check cannot run, for example due to missing credentials or services | Report the gap when discovered; restore access or get explicit human acceptance of the limitation before delivery |
| Failed | An applicable check ran and failed | Fix it and rerun; if blocked by an unrelated baseline failure, show the evidence and escalate rather than relabeling it |

For documentation-only work, inspect the content, references, and consistency, and run
documentation checks if the repository has them. Do not invent a build to create a gate.
If no automated checks exist, say so early and describe the manual evidence available.
If that evidence cannot establish the requested behavior, escalate the remaining gap
before delivery. Never present a missing, blocked, or failed check as a pass.

The reviewer receives this evidence and its limitations. The PR records unresolved gaps
and any explicit human acceptance. Missing automation never excuses independent review,
and a review acceptance never erases a verification failure.
