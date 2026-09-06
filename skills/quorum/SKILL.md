---
name: quorum
description: Turn a work item into a human-approved plan and a reviewed pull request, end to end. Two independent planner models propose approaches, a coordinator merges them, and a single implementer works against a reviewer of a different model. Use when the user wants both planning and implementation for a non-trivial change, or says "quorum" with no further qualification. For planning only, use quorum-plan; to implement an existing plan, use quorum-build.
user-invocable: true
---

# Quorum

Two independent models plan, a human approves, and one implementer works against a
read-only reviewer of a different model. Quorum opens a pull request. It never merges.

This skill composes `quorum-plan` and `quorum-build` without changing their defaults.

```
Independent plans -> Merge -> Human approval
                                  |
                                  v
Implement -> Relevant checks -> Independent review
    ^                                |
    +------------ REJECT ------------+
                                     | ACCEPT
                                     v
                           Remaining applicable checks -> Deliver
```

## Phase A - Plan

Invoke `quorum-plan`. If skill invocation is unavailable, follow
[../quorum-plan/SKILL.md](../quorum-plan/SKILL.md) directly.

It uses two planners, coordinator-led intake, and one merge by default. Focused
follow-ups or a third opinion address concrete unresolved disagreements, not a preset
profile. Let that skill own the human approval gate.

Pass the resulting `quorum/plans/approved-plan.md` path under the session artifacts
directory to the build phase without paraphrasing it. If it is missing or older than
the working draft `quorum/plans/plan.md`, do not build: the current draft needs approval.

## Phase B - Build

Invoke `quorum-build` with that approved-plan path. If skill invocation is unavailable,
follow [../quorum-build/SKILL.md](../quorum-build/SKILL.md) directly.

It discovers repository-native checks, implements against an independent reviewer,
and opens a pull request after acceptance and applicable verification. It does not
require a Makefile or introduce build tooling.

## Handling escapes

- A rejected plan stays in the planning phase.
- A review finding that requires scope beyond the approved plan goes back to the human
  or `quorum-plan`. Reopening planning invalidates the old approval; build resumes only
  after the revised plan is approved.
- If either half hits its stop condition, surface the state and open questions. Do not
  silently restart the phase.

## Invariants

- Planners and reviewers are read-only. Only the coordinator/implementer writes files.
- Reviewer and implementer use different models.
- Plans and verdicts persist under `quorum/` in the session artifacts directory.
- Verification follows
  [references/verification.md](references/verification.md), using the repository's own
  workflow and valid evidence rather than mandatory target names.
- A human owns the merge.
