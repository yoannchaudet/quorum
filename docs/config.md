# Configuration

Quorum reads a single YAML config, default `~/.quorum/config.yaml`. It is optional;
every key has a default. The path is overridable via a CLI flag.

Unknown keys are rejected. The global persistence model uses `data_dir`; the former
`state_dir` key is no longer supported.

## Schema

```yaml
# Root for the global database and per-work-item filesystem state.
data_dir: ~/.quorum

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

# Execution isolation (see isolation.md). Applied to every agent invocation.
sandbox:
  enabled: true                 # run agents inside Copilot's local sandbox
  experimental: true            # local sandbox currently requires --experimental
  allow_outbound: true          # permit internet-backed tools and browser navigation
  # Destructive tools denied even inside the sandbox (defense in depth).
  deny_tools:
    - shell(rm)
  browser:
    enabled: true
    headed: true                # visible browser when a graphical display is available
    package: "@playwright/mcp@0.0.79"

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
  step_timeout_secs: 1800
```

## Precedence

CLI flag > config file > built-in default.

Repository selection is runtime context rather than configuration:
`--context <folder>` > current working directory. See [repositories](repositories.md).

## Notes

- Model IDs are the only vendor-specific values; the *roster size and roles* are fixed in
  the [docs](agents.md).
- `reviewer` MUST differ from `implementer` for the adversarial loop to be meaningful.
- The unattended Coordinator uses the local sandbox; cloud sandboxes cannot run with
  the programmatic prompt interface.
- Browser automation uses a pinned Playwright MCP package, an isolated in-memory
  profile, and a work-item artifact directory. It never connects to the user's normal
  browser profile.
- Outbound internet is capability-first and enabled by default. Turning it off removes
  Copilot URL permission, but the effective shell network boundary also depends on the
  configured preview Local Sandbox policy.
