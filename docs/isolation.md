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

Quorum refuses to run a real agent when the Local Sandbox is disabled. Dry runs remain
available because they spawn no agent process.

## Per-role profiles

| Role | Filesystem | cwd | Rationale |
|------|-----------|-----|-----------|
| Planner | read-only | Work-item worktree root | Analysis only; plan captured from stdout. |
| Reviewer | read-only | Work-item worktree root | Reviews the real checkout; feedback captured from stdout. |
| Implementer | read/write | Work-item worktree root (`implementation/`) | Must modify the real checkout. |

Because the run is non-interactive (`--no-ask-user`), copilot cannot prompt for tool
approval and would otherwise deny every action. Tools are therefore granted up front,
**scoped by role**:

- **Implementer (read/write)**: `--allow-all-tools` — the sandbox is the
  boundary, so broad tools are allowed inside it. A **deny-list** still blocks destructive
  operations (e.g. `shell(rm)`) as defense in depth.
- **Planner / Reviewer (read-only)**: `--allow-tool read` only — they can inspect but not modify.

There is no blanket `--allow-all-tools` outside the sandbox.

Unattended runs also disable remote control/export, automatic updates, and the built-in
GitHub MCP server. Detected credential-like environment variables are passed through
Copilot's secret stripping so shell and local MCP subprocesses do not inherit them.

## Invocation shape

Each agent run is one non-interactive `copilot` call:

```
# Implementer (read/write): cwd = linked worktree root
copilot --sandbox --experimental --no-ask-user --allow-all-tools \
        --add-dir "<absolute common Git directory>" \
        --add-dir "<work-item runtime directory>" \
        --additional-mcp-config="@<runtime>/playwright-mcp.json" \
        --deny-tool "<destructive ops>" -p "<prompt>"

# Planner/Reviewer (read-only): cwd = read-only inputs
copilot --sandbox --experimental --no-ask-user --allow-tool read \
        --deny-tool "<destructive ops>" -p "<prompt>"
```

Prompts come from reviewable markdown files (see [prompts](prompts.md)). Filesystem,
network, and deny-tool policy come from the `sandbox:` config block (see [config](config.md)).

## Process and server lifetime

Each invocation runs in a dedicated process group and is supervised by the
Coordinator. `limits.step_timeout_secs` is enforced. On success, failure, or timeout,
Quorum terminates the process group so background development servers, browsers,
language servers, and other descendants cannot survive the step. Captured output is
bounded to its most recent 1 MiB per stream.

The Implementer may run ordinary repository commands without a Quorum command
allow-list. Development servers should bind to `127.0.0.1` and exist only long enough
for tests or browser validation within the same step.

## Browser isolation

The Implementer receives an official, pinned Playwright MCP sidecar. It runs under the
same sandbox and process group with:

- an isolated in-memory browser profile;
- a deterministic viewport;
- a work-item artifact output directory capped by Playwright;
- headed mode on graphical hosts and headless fallback otherwise;
- no connection to the user's browser, extensions, cookies, passwords, or history.

Screenshots and browser diagnostics are retained as work-item artifacts. The
Implementer may also use browser tooling already provided by the repository.

## Recovery interplay

The sandbox confines normal agent writes to the linked worktree. Its `.git` file points
to an external shared Git directory, resolved with
`git rev-parse --path-format=absolute --git-common-dir`. Quorum grants that exact path to
the Implementer with `--add-dir` so Git commands work in the linked checkout. Planners
and Reviewers remain
read-only and receive no additional writable directory. Combined with recoverable
worktree setup and the per-round protocol, this keeps each step resumable (see
[persistence](persistence.md)).

Local Sandbox and MXC are preview technologies and are not virtual-machine-grade
isolation. With outbound internet enabled, code from a malicious repository can
transmit repository files it is allowed to read. The boundary protects the rest of the
host and personal credentials; it does not make readable source code confidential.

## Cloud sandbox (future)

Copilot's **cloud sandbox** (`--cloud`) offers stronger, fully isolated ephemeral Linux
environments, but **cannot** be combined with `-p`/`-i` — it is interactive-only, so the
unattended Coordinator cannot use it. It is a possible future option for interactive
human intervention
([sessions](sessions.md)); it is not specified here.
