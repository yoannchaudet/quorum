<p align="center">
  <img src="icons/icon.svg" alt="Quorum logo" width="120" height="120" />
</p>

# Quorum

Quorum is my personal, lightweight harness for leveraging AI. It is a **Copilot skill**
that standardizes how I turn "work items" into plans that later get implemented and
reviewed.

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

## Install

```bash
script/install            # symlink into ~/.copilot/skills (edits take effect live)
script/install --copy     # copy instead
script/install --uninstall
```

Then invoke `/quorum` in Copilot, or just ask for a fleet-planned change.

## How it works

```
Intake ──> Fleet planning ──> Converge ──┐
   ^            ^                        │ ITERATE
   │            └────────────────────────┘
   │                    │ CONVERGED
   │                    v
   └── answers ──  Plan gate (human) ──┐ reject
                        │ approve      │
                        v              └──> Fleet planning
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
roster is one Claude, one GPT, one Gemini; it is overridable per run.

**Human gates.** Planners ask the minimum questions that would change the plan, and the
converged plan needs an explicit approval before any code is written.

**Adversarial review.** The implementer works against a `rubber-duck` reviewer pinned to
a different model, which is told to assume the work is wrong and go find the evidence. It
returns `ACCEPT` or `REJECT` with concrete findings. The loop terminates on an accept, a
round cap, or a stuck git tree.

**Two verification loops.** Every repo Quorum touches must expose `make verify` (fast,
hermetic — the inner loop, run before every single review) and `make verify-full` (slow —
run once, after the reviewer accepts). If they are missing, Quorum bootstraps them from a
template and asks before proceeding.

Quorum opens a pull request. It never merges it.

## Layout

```
skills/quorum/
  SKILL.md                 the state machine the coordinator follows
  references/planning.md   fleet roster, planner prompts, convergence rules
  references/review.md     adversarial reviewer prompt, loop and stop conditions
  references/makefile.md   the verify / verify-full contract and templates
script/install             install into ~/.copilot/skills
```

## History

Quorum was previously a Rust core with a CLI and a Tauri frontend. The models got good
enough that the harness was mostly ceremony, so it was scrapped in favor of the skill —
the prompts and the state machine were always the valuable part. The old implementation
lives in this repository's git history.
