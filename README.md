<p align="center">
  <img src="icons/icon.svg" alt="Quorum logo" width="120" height="120" />
</p>

# Quorum

Quorum is my personal, lightweight harness for leveraging AI. It is a set of **Copilot
skills** that standardize how I turn "work items" into plans that later get implemented
and reviewed.

Nothing fancy, nothing magic — just some standardization and light automation. As AI
models progress they get slower, and the development life cycle suffers. Quorum fills
some of that gap by automating the menial tasks.

Where does the name come from? I believe all models will eventually converge, and that
any of them can do a pretty good job of following a plan. Planning, to me, is one of the
most important steps — like writing the specification for a piece of work: with a good
foundation, the work goes well. So the planning phase is delegated to multiple models
(hence "Quorum"), and the overall plan is pieced together from all their ideas. A single
implementer then works against an adversarial reviewer (a different model). Each phase is
a loop. Humans stay involved at intake, and to approve the finalized plan.

## The three skills

The pipeline has two halves, and each half is its own skill so you can run just the part
you need. The full pipeline is worth its ceremony for a change you would otherwise design
badly; it is pure overhead for a one-file fix.

| Skill | Does | Use it when |
|---|---|---|
| `/quorum` | Plan **and** build, end to end | The change is non-trivial and you want the whole machine |
| `/quorum-plan` | Plan only — stops at an approved plan, writes no code | You want a spec, or a second opinion on an approach |
| `/quorum-build` | Build only — implements a plan or work item under adversarial review | The work is already specified |

The halves run at **lighter defaults** on their own than they do under `/quorum`:

| | standalone (`light`) | under `/quorum` (`full`) |
|---|---|---|
| Planner models | 2 | 3 |
| Intake | coordinator asks directly | one intake sub-agent per planner |
| Convergence rounds | 1 | 3 |
| Review rounds | 3 | 5 |
| `make verify` / `verify-full` | used if present, else the repo's own equivalents | required, bootstrapped if missing |

What `light` does *not* relax: planning is still a fleet of independent models, the plan
still needs human approval, the reviewer still runs and is still a different model from
the implementer, both verification loops still gate, and the pull request is still never
merged. A repo that offers no usable gate at all can only proceed on an explicit waiver,
agreed before any code is written and disclosed in the PR.

`/quorum-plan` hands off to `/quorum-build` through `quorum/plans/approved-plan.md` in the
session's artifacts directory — a file written only when the human approves, and deleted
whenever planning reopens — so the two compose without going through `/quorum`, and
neither an unapproved nor a superseded plan can be built by accident.

## Install

```bash
script/install            # symlink all skills into ~/.copilot/skills (edits take effect live)
script/install --copy     # copy instead
script/install --uninstall
```

Then invoke `/quorum`, `/quorum-plan`, or `/quorum-build` in Copilot — or just ask for a
fleet-planned change.

## How it works

```
quorum-plan
    Intake ──> Fleet planning ──> Converge ──┐
       ^            ^                        │ ITERATE
       │            └────────────────────────┘
       │                    │ CONVERGED
       │                    v
       └── answers ──  Plan gate (human) ──┐ reject
                            │ approve      │
                            │              └──> Fleet planning
                            v
                 quorum/plans/approved-plan.md   <── the handoff
                            │
                            v
quorum-build
                    Implement ──> make verify ──> Adversarial review
                        ^                              │
                        └──────── REJECT ──────────────┤
                                                       │ ACCEPT
                                                       v
                                                make verify-full
                                                       │ pass
                                                       v
                                                    Deliver
```

**Fleet planning.** Several planner models run in parallel, each in isolation — no
planner ever sees another's output. A coordinator merges the candidates into one plan,
then feeds it back to the same planners until the plan stops materially changing. Default
roster is one Claude, one GPT, and — at `full` — one Gemini; it is overridable per run.

**Human gates.** Planners ask the minimum questions that would change the plan, and the
converged plan needs an explicit approval before any code is written.

**Adversarial review.** The implementer works against a `rubber-duck` reviewer pinned to
a different model, which is told to assume the work is wrong and go find the evidence. It
returns `ACCEPT` or `REJECT` with concrete findings. The loop terminates on an accept, a
round cap, or a stuck git tree.

**Two verification loops.** A fast, hermetic inner loop runs before every single review; a
slow one runs once after the reviewer accepts. `make verify` and `make verify-full` are
the contract. `/quorum` requires both, and `/quorum-build` bootstraps them from a template
when they are missing; run standalone it will instead substitute the repo's own fast
checks and its fullest build-and-test run, and tell you what it picked.

Quorum opens a pull request. It never merges it.

## Layout

```
skills/
  quorum/
    SKILL.md                 orchestrator: runs both halves at full strength
    references/makefile.md   the shared verify / verify-full contract
  quorum-plan/
    SKILL.md                 intake, fleet planning, convergence, plan gate
    references/planning.md   roster, planner prompts, merge and convergence rules
  quorum-build/
    SKILL.md                 implement, verify, adversarial review, deliver
    references/review.md     reviewer prompt, loop and stop conditions
script/install               install every skill into ~/.copilot/skills
```

## History

Quorum was previously a Rust core with a CLI and a Tauri frontend. The models got good
enough that the harness was mostly ceremony, so it was scrapped in favor of the skill —
the prompts and the state machine were always the valuable part. The old implementation
lives in this repository's git history.
