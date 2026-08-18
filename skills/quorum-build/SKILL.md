---
name: quorum-build
description: Implement a plan or work item and drive it to a reviewed pull request. A single implementer works against an adversarial reviewer pinned to a different model, with a fast verification loop before every review and a slow one before delivery. Use when asked to implement an approved plan, build out an issue with real review, or when the user says "quorum build". To produce the plan first, use quorum-plan; for both halves end to end, use the quorum skill.
user-invocable: true
---

# Quorum — build

One implementer, one **adversarial reviewer** of a different model, and two verification
loops. A model reviewing its own work rationalizes it; a different model, told to assume
the work is wrong, does not.

This skill starts from a spec — ideally a plan from `quorum-plan`, otherwise the work item
itself — and stops at an open pull request. It never merges.

You are the **Implementer** and you own the loop below.

```
Resolve spec ──> Implement ──> make verify ──> Adversarial review
                     ^                                │
                     └──────── REJECT ────────────────┤
                                                      │ ACCEPT
                                                      v
                                               make verify-full
                                                      │ pass
                                                      v
                                                   Deliver
```

## Profile

Read the caller's profile. Default to **light** unless a caller (normally the `quorum`
skill) explicitly says `full`.

| | `light` (default) | `full` |
|---|---|---|
| Review rounds cap | 3 | 5 |
| Makefile contract | Use `verify` / `verify-full` if present. If missing, offer to bootstrap but do not require it — substitute the repo's own commands for both loops, and tell the human which ones you picked | Required. Bootstrap from the template and get an explicit OK before writing code |
| Fast loop | `make verify`, else the repo's test + lint + typecheck commands | `make verify` |
| Slow loop | `make verify-full`, else the repo's fullest available check — full build plus a non-short test run | `make verify-full` |
| Everything else | Identical | Identical |

`light` is the small-task path. What it does **not** relax: the reviewer still runs, the
reviewer is still a different model, a fast loop still gates every review, a slow loop
still gates delivery, and the pull request is still never merged. Escalate to `full`
mid-run if the change turns out to be larger than it looked, and say so when you do.

## Phase 0 — Resolve the spec

Work out what you are building, in this order, and state out loud which source you used:

1. `quorum/plans/approved-plan.md` under the session artifacts dir — a human-approved
   handoff from `quorum-plan`.
2. An explicit plan file path or URL the human gave you.
3. A plan section inside the referenced GitHub issue or pull request body
   (`gh issue view <n>`, `gh pr view <n>`).
4. **No plan at all** — treat the work item itself as the spec. Echo your understanding of
   the scope back to the human **once** before writing any code, so a misread is caught
   before it becomes a diff. Do not turn this into a planning session; if the work item is
   too vague to restate in a few lines, stop and suggest `quorum-plan`.

**Guards.** An approval only applies to the draft the human actually saw, so before you
trust `approved-plan.md`, check both of these:

- `quorum/plans/plan.md` exists but there is no `approved-plan.md` → that plan was merged
  but never approved; it may be a draft, or one the human rejected.
- `approved-plan.md` is **older than** `plan.md` → a newer draft exists and this approval
  is stale, typically because planning reopened after a rejection or scope drift.

In either case, do not build. Show the current draft, get an explicit approval, and only
then treat it as the spec.

Persist what you resolved so a crashed or resumed session can pick it up: if the spec did
not already come from `quorum/plans/approved-plan.md`, write it there (or to
`quorum/work-item.md` in case 4). Create `quorum/reviews/` for the verdicts.

## Phase 1 — Check the verification contract

The loop needs a fast gate and a slow gate — see
[../quorum/references/makefile.md](../quorum/references/makefile.md).

```bash
make -n verify >/dev/null 2>&1 && echo "verify: ok" || echo "verify: MISSING"
make -n verify-full >/dev/null 2>&1 && echo "verify-full: ok" || echo "verify-full: MISSING"
```

In `full`, both must exist before you write code; bootstrap them from the template and get
an explicit OK. **This skill is the sole owner of the contract** — no other phase
bootstraps it.

In `light`, use the targets if they are there. If they are not, do not bootstrap unless
the human asks; instead pick substitutes from the repository itself and name them
explicitly, to the human and to the reviewer:

- **Fast loop** — the repo's test runner, linter, and type checker, in their quickest form.
- **Slow loop** — the fullest check the repo offers: a real build plus a non-short test
  run, including integration or e2e suites if they exist.

Both loops must exist in some form. If the repository genuinely offers nothing you can use
as a gate, that is not something to discover at delivery time: say so **before** writing
code and ask the human to either let you bootstrap the Makefile contract or explicitly
waive the gate for this run. A waived run is allowed, but record the waiver in the pull
request body — the reviewer is then carrying the entire burden and the human should know
it. Never reach the end of a run and quietly ship with no verification.

## Phase 2 — Implement and adversarially review

Follow the spec. If a step is wrong or infeasible, do the smallest correct thing and
record the deviation. Keep changes scoped to the spec.

Each round, in order:

1. Implement (first round) or fix the reviewer's findings (later rounds).
2. Run the **fast loop** — `make verify`, or the substituted commands. Never hand work to
   the reviewer while it fails.
3. Run the **adversarial reviewer**: a `rubber-duck` sub-agent pinned to a model different
   from your own, which must return `ACCEPT` or `REJECT` plus concrete findings.
4. `REJECT` → go to 1. `ACCEPT` → run the **slow loop** — `make verify-full`, or its
   substitute — once. If it fails, the failure becomes findings and you go back to 1.

A loop that was waived in Phase 1 is skipped in the step above, and only there. The
reviewer still runs every round regardless; a waiver excuses a missing gate, never the
review.

Stop conditions, both of which escalate to the human rather than looping forever: the
profile's round cap (**3** in `light`, **5** in `full`), or two consecutive rejected rounds
producing an identical git tree.

Full reviewer prompt, verdict format, and loop rules in
[references/review.md](references/review.md).

## Phase 3 — Deliver

Commit with a message describing the work item, push the branch, and open a pull request
whose body links the work item, the spec, the reviewer's final verdict, and any gate
waived in Phase 1. **Never merge it.** A human owns the merge.

## Rules that hold across every round

- The reviewer is **read-only**. Only you write files.
- Reviewer and implementer must be **different models**. If they would collide, pick
  another reviewer model from a different vendor.
- Every review and verdict is a file under `quorum/reviews/`, so a crashed or resumed
  session can pick up where it left off.
- The fast loop runs before every review; the slow loop only after an `ACCEPT`. A loop
  exists unless the human waived it in Phase 1, and a waiver is only ever granted before
  code is written.
- Never suppress a test, widen a lint exclusion, or weaken an assertion to make the loop
  go green.
