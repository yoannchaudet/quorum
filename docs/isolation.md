# Execution Isolation

How Quorum runs `copilot` agents (PL, IM, RV) safely. This is **execution isolation** —
distinct from the **idea isolation** that keeps PLs from seeing each other's output
(see [agents](agents.md)).

The CO runs **unattended**, so agents run **non-interactively**: there is no human to
answer approval prompts. We do **not** grant blanket `--yolo`. Instead we use GitHub
Copilot's **local sandbox** as the OS-level boundary and give each role a scoped profile.

## Local Sandbox (LS)

Quorum invokes agents with Copilot's local sandbox (`--sandbox --experimental`), an
OS-level boundary (macOS Seatbelt / Linux bubblewrap). It:

- confines filesystem writes to the process **cwd** and temp; home and system are
  read-only; everything else is blocked;
- controls network and credential exposure via policy;
- **works programmatically** (`copilot --sandbox -p "…"`), which is what lets the
  unattended CO use it.

The **CO itself (the Rust Core) is not sandboxed** — it is the orchestrator. Only the
`copilot` agent invocations it spawns are sandboxed.

## Per-role profiles

| Role | Filesystem | cwd | Rationale |
|------|-----------|-----|-----------|
| PL | read-only | read-only view of WI + assets | Analysis only; plan captured from stdout. |
| RV | read-only | read-only view of IM output | Reviews; feedback captured from stdout. |
| IM | read/write | its WI workspace (`implementation/`) | Must write code; writes stay in the workspace. |

Because the run is non-interactive (`--no-ask-user`), copilot cannot prompt for tool
approval and would otherwise deny every action. Tools are therefore granted up front,
**scoped by role**:

- **IM (read/write)**: with the sandbox enabled, `--allow-all-tools` — the sandbox is the
  boundary, so broad tools are allowed inside it. A **deny-list** still blocks destructive
  operations (e.g. `shell(rm)`) as defense in depth. If the sandbox is **disabled**, the IM
  is instead scoped to file tools (`--allow-tool read,write`) — never blanket tools without
  an OS boundary.
- **PL / RV (read-only)**: `--allow-tool read` only — they can inspect but not modify.

There is no blanket `--allow-all-tools` outside the sandbox.

## Invocation shape

Each agent run is one non-interactive `copilot` call:

```
# IM (read/write): cwd = implementation/, sandbox allows writes here + temp
copilot --sandbox --experimental --no-ask-user --allow-all-tools \
        --deny-tool "<destructive ops>" -p "<prompt>"

# PL/RV (read-only): cwd = read-only inputs
copilot --sandbox --experimental --no-ask-user --allow-tool read \
        --deny-tool "<destructive ops>" -p "<prompt>"
```

Prompts come from reviewable markdown files (see [prompts](prompts.md)). Filesystem,
network, and deny-tool policy come from the `sandbox:` config block (see [config](config.md)).

## Recovery interplay

The sandbox confines every agent's writes to the WI workspace, so a crashed or killed
agent can never leave partial state outside `<state_dir>/<wi-id>/`. Combined with atomic
step transactions, this keeps each step recoverable (see [persistence](persistence.md)).

## Cloud sandbox (future)

Copilot's **cloud sandbox** (`--cloud`) offers stronger, fully isolated ephemeral Linux
environments, but **cannot** be combined with `-p`/`-i` — it is interactive-only, so the
unattended CO cannot use it. It is a possible future option for interactive HI
([sessions](sessions.md)); it is not specified here.
