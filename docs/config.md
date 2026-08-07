# Configuration

Quorum reads a single YAML config, default `~/.quorum/config.yaml`. It is optional;
every key has a default. The path is overridable via a CLI flag.

## Schema

```yaml
# Where per-WI state and assets live (see persistence.md).
state_dir: ~/.quorum/state

# Planner roster override (see agents.md). Keys are slots; values are model IDs.
planners:
  planner-a: <model-id>
  planner-b: <model-id>
  planner-c: <model-id>

# Model targets for the other roles.
models:
  implementer: <model-id>
  reviewer: <model-id>          # MUST differ from implementer
  coordinator: <model-id>       # used for merge/convergence prompts

# Human-review gates (see state-machine.md).
reviews:
  plan_review: true             # PlanReview gate on/off
  work_review: true             # WorkReview gate on/off

# Loop bounds and resilience (see persistence.md).
limits:
  convergence_max_iters: 5
  convergence_diff_threshold: 0.1
  adversarial_max_iters: 5
  step_retries: 3
  step_timeout_secs: 600
```

## Precedence

CLI flag > config file > built-in default.

## Notes

- Model IDs are the only vendor-specific values; the *roster size and roles* are fixed in
  the [docs](agents.md).
- `reviewer` MUST differ from `implementer` for the adversarial loop to be meaningful.
