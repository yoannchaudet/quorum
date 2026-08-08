# Agents

Four roles drive a work item. All are AI models invoked through the `copilot` CLI. The
Coordinator is the only stateful orchestrator; Planners, the Implementer, and the
Reviewer are stateless workers given explicit inputs.

## Coordinator

- Owns the [state machine](state-machine.md) for one work item.
- Runs Planners, merges their output, and drives convergence.
- Runs the adversarial Implementer and Reviewer loop.
- Pulls humans in via [Sessions](sessions.md) when blocked.
- Persists after every step so it can resume after a crash (see [persistence](persistence.md)).
- Emits typed activity before and after long-running work so frontends can show live
  progress without parsing agent output (see [observability](observability.md)).

## Planners — the quorum

- Each Planner produces a **candidate plan** for the work item **in isolation**: it
  sees the work item and human answers, but never another Planner's output. This keeps ideas independent.
  (Distinct from *execution isolation* — see below.)
- Planners may raise **follow-up questions**. If any do, the Coordinator enters
  `IntakeReview`, collects answers, and re-runs the Planners.

### Default roster

A fixed default set of Planners ships in the specs, **overridable** in [config](config.md)
(`planners:`). Default roster:

| Slot | Purpose |
|------|---------|
| `planner-a` | Primary generalist model. |
| `planner-b` | Second independent model, different vendor/family. |
| `planner-c` | Third independent model for tie-breaking / breadth. |

Exact model IDs are set in config; the docs fix the *number and roles*, not the vendors.

## Convergence loop

1. `Planning`: all Planners produce candidate plans independently.
2. `Converging`: the Coordinator merges candidates into one Plan.
3. Convergence criteria — the Coordinator accepts the Plan when **both**:
   - no Planner raised a new open question, and
   - the merged Plan is **stable**: re-running Planners against the current merged Plan yields
     no material changes (a diff below the configured threshold), OR the configured
     max iterations is reached.
4. If not converged, loop back to `Planning` feeding the merged Plan as context.

## Implementer and Reviewer

- The Implementer produces the implementation from the accepted Plan.
- The Reviewer is a **different** model that adversarially reviews the Implementer's output.
- **Adversarial loop**: `Implementing` → `Reviewing` → `Implementing` until the Reviewer accepts
  or the Coordinator forces `WorkReview` because two rejected rounds have the same Git tree or
  the maximum iteration count is reached.
- The Coordinator, not the Implementer, stages and commits each changed round. Empty rounds retain the
  existing commit while recording its tree SHA.
- The Implementer can run repository tests and builds, start a loopback development
  server, automate an isolated headless or visible browser, and retain screenshots.
- All Implementer child processes are step-scoped and terminated before the
  Coordinator finalizes the round.

### Human-approved execution capabilities

Every merged Plan contains an exhaustive `Execution capabilities` YAML section. At
`PlanReview`, approving the Plan also approves that exact grant:

- shell execution;
- outbound internet;
- a loopback development server;
- no browser, a headless browser, or a visible isolated browser;
- retained artifacts;
- a per-step timeout.

Quorum parses and validates the section before accepting the Plan. The runtime sandbox
is generated from that persisted grant. Capabilities omitted from the Plan are not
available to the Implementer. Browser access requires internet and artifact
authorization because the trusted browser sidecar cannot run inside the network
sandbox.

## Human intervention

The Coordinator pulls humans in at exactly three points, each a blocked state:

| State | What the human does |
|-------|---------------------|
| `IntakeReview` | Answers Planner follow-up questions. |
| `PlanReview` (optional) | Approves the converged Plan, or rejects it with optional feedback for the next planning pass. |
| `WorkReview` (optional) | Approves the accepted work, or rejects it with optional feedback for the next implementation pass. |

All human intervention happens through a resumable [Session](sessions.md). All prompts
the Coordinator gives any
agent live as reviewable markdown files (see [prompts](prompts.md)).

## Execution isolation

Every agent runs inside a Local Sandbox, non-interactively, with a
per-role filesystem profile. This is separate from the *idea isolation* above.

| Role | Filesystem |
|------|-----------|
| Planner | read-only |
| Reviewer | read-only |
| Implementer | read/write, confined to its work-item workspace |

Full details — invocation flags, deny-list, network policy — are in
[isolation](isolation.md).
