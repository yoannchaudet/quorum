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

All three entry points use the same defaults: two independent planners, coordinator-led
intake, one merge, and at most three implementation/review rounds. `/quorum` simply
composes the halves. Focused planning follow-ups or a third opinion address concrete
unresolved disagreements; they are not routine extra rounds.

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
    Intake -> Two independent plans -> Merge
                                        |
                              Focused follow-up if needed
                                        |
                                 Human approval
                                        |
                        quorum/plans/approved-plan.md
                                        |
                                        v
quorum-build
    Implement -> Relevant checks -> Independent review
        ^                                |
        +------------ REJECT ------------+
                                         | ACCEPT
                                         v
                               Remaining applicable checks -> Deliver
```

**Independent planning.** Two planner models run in parallel, each without seeing the
other's candidate. A coordinator merges their reasoning once. If a material disagreement
needs more evidence, focused follow-ups or a third opinion address that decision, with
at most two follow-up rounds before escalating to the human. Material dissent stays
visible; agreement is not a gate. The default roster is one Claude and one GPT.

**Human gates.** The coordinator asks only blocking questions, and the merged plan needs
explicit approval before any code is written.

**Independent review.** The implementer works against a read-only reviewer pinned to
a different model, which investigates the actual code for concrete defects. It
returns `ACCEPT` or `REJECT` with concrete findings. The loop terminates on an accept, a
round cap, or a stuck git tree.

**Repository-native verification.** Quorum discovers checks from repository instructions,
scripts, and CI. Relevant checks run before review; remaining broader applicable checks
run before delivery. These are two verification moments, not two mandatory suites.
Successful evidence can be reused while its covered inputs and environment are unchanged.
Not-applicable, unavailable, and failed checks are distinguished and gaps are surfaced.
Quorum does not add Makefiles, aliases, or build tooling unless asked.

Quorum opens a pull request. It never merges it.

## Layout

```
skills/
  quorum/
    SKILL.md                 orchestrator: composes both halves
    references/verification.md  shared repository-native verification guidance
  quorum-plan/
    SKILL.md                 intake, independent planning, merge, plan gate
    references/planning.md   planner prompts, merge and focused follow-ups
  quorum-build/
    SKILL.md                 implement, verify, independent review, deliver
    references/review.md     reviewer prompt, loop and stop conditions
script/install               install every skill into ~/.copilot/skills
```

## History

Quorum was previously a Rust core with a CLI and a Tauri frontend. The models got good
enough that the harness was mostly ceremony, so it was scrapped in favor of the skill —
the prompts and the state machine were always the valuable part. The old implementation
lives in this repository's git history.
