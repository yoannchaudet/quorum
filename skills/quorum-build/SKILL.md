---
name: quorum-build
description: Implement a plan or work item and drive it to a reviewed pull request. One implementer works against a read-only reviewer of a different model, using repository-native verification before review and delivery. Use when asked to implement an approved plan, build out an issue with real review, or when the user says "quorum build". To produce the plan first, use quorum-plan; for both halves end to end, use quorum.
user-invocable: true
---

# Quorum - build

You are the implementer. Follow the spec, gather verification evidence, and work against
a read-only reviewer of a different model. Stop at an open pull request. Never merge.

## Phase 0 - Resolve the spec

Resolve what to build in this order and state which source you used:

1. `quorum/plans/approved-plan.md` under the session artifacts directory.
2. An explicit plan path or URL from the human.
3. A plan in the referenced GitHub issue or pull request.
4. The work item itself. Restate the scope once before editing; if it is too vague to
   restate in a few lines, stop and suggest `quorum-plan`.

Before falling back to another source, check for a local `quorum/plans/plan.md` draft.
If it exists without `approved-plan.md`, or the approved file is older than the draft,
show the current draft and obtain explicit approval before building. Do not bypass an
unapproved or superseded plan.

Persist an externally supplied spec in `quorum/spec.md`, or an unplanned work item in
`quorum/work-item.md`. Do not manufacture an approved-plan artifact for an external
source. Create `quorum/reviews/` for verdicts.

## Phase 1 - Select verification

Follow [../quorum/references/verification.md](../quorum/references/verification.md).
Inspect repository instructions, scripts, and CI. Record the relevant pre-review checks,
any broader pre-delivery checks, and known gaps in `quorum/verification.md`.

Use existing commands. Do not require Make targets or bootstrap tooling unless asked.
The two moments can use the same suite; successful evidence can be reused while its
covered inputs and relevant environment remain unchanged.

Distinguish not-applicable checks from unavailable or failed ones. State missing
automation early and identify manual evidence. Escalate gaps that prevent establishing
the requested behavior rather than quietly delivering without evidence.

## Phase 2 - Implement and review

Keep changes scoped to the spec and honor repository conventions and instruction files
such as `AGENTS.md`. If a step is wrong or infeasible, take the smallest correct approach
within the approved scope and record the deviation. Changes to scope or the core
approach require human approval before implementation. Each round:

1. Implement or address the reviewer's findings.
2. Run relevant checks and update the verification evidence. Do not hand known failing
   checks to review; investigate or escalate them.
3. Run a read-only reviewer pinned to a model different from your own. It returns
   `ACCEPT` or `REJECT` with concrete findings.
4. On `REJECT`, address findings and repeat. On `ACCEPT`, run any remaining applicable
   pre-delivery checks. A failure returns to the fix-and-review loop.

Review is always required, even without automated checks. Explain any disputed finding
in the next round instead of silently ignoring it.

Cap the loop at three review rounds. Stop earlier if two consecutive rejected rounds
have the same working-tree contents, or a broader check fails for the same reason twice.
Escalate with the evidence and open findings; do not silently restart the loop.

See [references/review.md](references/review.md) for the reviewer prompt and stop rules.

## Phase 3 - Deliver

After reviewer acceptance and applicable verification, commit, push, and open a pull
request. Include the work item, spec or a portable summary, final review verdict,
verification evidence, and unresolved limitations with any human acceptance. Do not
rely on local artifact paths as links other readers can access. Never merge.

## Invariants

- Only the implementer writes files. The reviewer is read-only.
- Reviewer and implementer use different models; prefer different vendors.
- Persist every verdict in `quorum/reviews/`.
- Reuse evidence only while valid; fixes invalidate affected checks and require review.
- Never suppress tests, widen exclusions, or weaken assertions to make checks pass.
- Scope changes go back to the human or `quorum-plan`, not straight into implementation.
