# Frontend Contract

The Core (`quorum-core`) is the whole product; a **frontend** only parses input, renders
output, and drives one work item through the Core API. The CLI is the current reference
driver; the Tauri UX is the intended human frontend. Both use the same entry points
below — no business logic lives in a frontend (see [architecture.md](architecture.md)).

## Driving a work item

| Concern | Core entry point |
|---|---|
| Resolve a repository-scoped ref/prefix to a work item | `Database::resolve_work_item` |
| List work items in a repository | `Database::work_items` |
| Register / look up a repository | `Database::register_repository`, `Database::registered_repository` |
| Prepare the on-disk checkout | `ensure_worktree` |
| Build the orchestrator | `Coordinator::new` |
| Run autonomously until blocked/terminal | `Coordinator::run_until_blocked` (or `step`) |
| Resolve a human gate | `Coordinator::resolve(Decision)` |
| Read current state / history / questions / PR URL | `Coordinator::state`, `history`, `questions`, `delivery_url` |
| Named human-intervention session | `Coordinator::session_name`, `ensure_session` |
| Render status | `StatusSnapshot::load` (+ focused Plan/Implementation documents) |

## Injection points (builders on `Coordinator`)

| Builder | Purpose |
|---|---|
| `with_observer` | Receive typed `ActivityEvent`s (live progress). |
| `with_cancel_token` | Install a `CancelToken` for a graceful, state-preserving stop. |
| `with_delivery_backend` | Coordinator-owned Git/GitHub delivery (or `DryRunDelivery`). |
| `with_implementation_allowed_dirs` | Extra sandbox read/write dirs for the Implementer. |

The agent backend itself is the `AgentRunner` trait (`CopilotRunner`, `EchoRunner`, or a
future SDK-backed runner) passed to `Coordinator::new`.

## Live activity

`run_until_blocked` is **blocking**. A frontend runs it on a worker thread and forwards
`ActivityEvent`s to its UI. Two ready-made observers exist:

- `CallbackObserver::new(|event| …)` — forward events to any closure (`Send + Sync`).
- `channel_observer()` — returns a boxed observer plus an `mpsc::Receiver<ActivityEvent>`
  to drain from the UI thread.

Every event is also persisted, so a frontend may poll history instead of subscribing.

## Cancellation

Hold a `CancelToken` (via `with_cancel_token`, or clone `Coordinator::cancel_token()`).
Calling `CancelToken::cancel()` stops `run_until_blocked` between steps and interrupts an
in-flight agent process; the call returns `CoordinatorError::Cancelled` and the persisted
state is untouched, so the work item resumes cleanly.

## Config

`Config::load(path)` reads `~/.quorum/config.yaml` (missing file → defaults). A frontend
that edits config persists it with `Config::save(path)`, which validates before writing
and creates parent directories.
