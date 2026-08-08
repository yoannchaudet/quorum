# Execution Isolation

How Quorum runs `copilot` agents safely. This is **execution isolation** —
distinct from the **idea isolation** that keeps Planners from seeing each other's output
(see [agents](agents.md)).

The Coordinator runs **unattended**, so agents run **non-interactively**: there is no human to
answer approval prompts. We do **not** grant blanket `--yolo`. Instead we use GitHub
Copilot's **local sandbox** as the OS-level boundary and give each role a scoped profile.

## Local Sandbox

Quorum invokes agents with Copilot's local sandbox (`--sandbox --experimental`), an
OS-level boundary (macOS Seatbelt / Linux bubblewrap). It:

- confines filesystem writes to the process **cwd** and temp; home and system are
  read-only; everything else is blocked;
- controls network and credential exposure via policy;
- **works programmatically** (`copilot --sandbox -p "…"`), which is what lets the
  unattended Coordinator use it.

The **Coordinator itself (the Rust Core) is not sandboxed** — it is the orchestrator. Only the
`copilot` agent invocations it spawns are sandboxed.

## Per-role profiles

| Role | Filesystem | cwd | Rationale |
|------|-----------|-----|-----------|
| Planner | read-only | Work-item worktree root | Analysis only; plan captured from stdout. |
| Reviewer | read-only | Work-item worktree root | Reviews the real checkout; feedback captured from stdout. |
| Implementer | read/write | Work-item worktree root (`implementation/`) | Must modify the real checkout. |

Because the run is non-interactive (`--no-ask-user`), copilot cannot prompt for tool
approval and would otherwise deny every action. Tools are therefore granted up front,
**scoped by role**:

- **Implementer (read/write)**: with the sandbox enabled, `--allow-all-tools` — the sandbox is the
  boundary, so broad tools are allowed inside it. A **deny-list** still blocks destructive
  operations (e.g. `shell(rm)`) as defense in depth. If the sandbox is **disabled**, the Implementer
  is instead scoped to file tools (`--allow-tool read,write`) — never blanket tools without
  an OS boundary.
- **Planner / Reviewer (read-only)**: `--allow-tool read` only — they can inspect but not modify.

There is no blanket `--allow-all-tools` outside the sandbox.

## Invocation shape

Each agent run is one non-interactive `copilot` call:

```
# Implementer (read/write): cwd = linked worktree root
copilot --sandbox --experimental --no-ask-user --allow-all-tools \
        --add-dir "<absolute common Git directory>" \
        --deny-tool "<destructive ops>" -p "<prompt>"

# Planner/Reviewer (read-only): cwd = read-only inputs
copilot --sandbox --experimental --no-ask-user --allow-tool read \
        --deny-tool "<destructive ops>" -p "<prompt>"
```

Prompts come from reviewable markdown files (see [prompts](prompts.md)). Filesystem,
network, and deny-tool policy come from the `sandbox:` config block (see [config](config.md)).

## Recovery interplay

The sandbox confines normal agent writes to the linked worktree. Its `.git` file points
to an external shared Git directory, resolved with
`git rev-parse --path-format=absolute --git-common-dir`. Quorum grants that exact path to
the Implementer with `--add-dir` so Git commands work in the linked checkout. Planners
and Reviewers remain
read-only and receive no additional writable directory. Combined with recoverable
worktree setup and the per-round protocol, this keeps each step resumable (see
[persistence](persistence.md)).

## Cloud sandbox (future)

Copilot's **cloud sandbox** (`--cloud`) offers stronger, fully isolated ephemeral Linux
environments, but **cannot** be combined with `-p`/`-i` — it is interactive-only, so the
unattended Coordinator cannot use it. It is a possible future option for interactive
human intervention
([sessions](sessions.md)); it is not specified here.
