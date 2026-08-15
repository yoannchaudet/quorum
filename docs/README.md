# Quorum Documentation

Specifications for Quorum's **Core** and **CLI**. The Core is implemented (Rust
`quorum-core` crate, unit-tested); the CLI is a thin **reference driver** used to
exercise the Core and will be superseded by the Tauri UX.

## Reading order

1. [glossary.md](glossary.md) — terms and acronyms (read first).
2. [architecture.md](architecture.md) — Core vs CLI, Rust workspace, future Tauri.
3. [work-items.md](work-items.md) — work-item inputs, GitHub pull, and images.
4. [repositories.md](repositories.md) — repository context and registration.
5. [cli.md](cli.md) — canonical command hierarchy and focused views.
6. [state-machine.md](state-machine.md) — the backbone: states, loops, and human intervention.
7. [agents.md](agents.md) — Coordinator, Planner quorum, Implementer, and Reviewer.
8. [isolation.md](isolation.md) — execution isolation: local sandbox + per-role profiles.
9. [prompts.md](prompts.md) — prompts as markdown files with frontmatter.
10. [sessions.md](sessions.md) — human intervention through resumable `copilot` sessions.
11. [config.md](config.md) — `~/.quorum/config.yaml` schema.
12. [persistence.md](persistence.md) — crash resilience and recovery.
13. [observability.md](observability.md) — live progress and rich work-item status.
14. [dev-lifecycle.md](dev-lifecycle.md) — Makefile: `verify` / `verify-full`.
15. [frontend.md](frontend.md) — the Core API contract a frontend (CLI or UX) drives.

## Doc style rules

- **Non-verbose.** Straight to the point. Prefer tables and diagrams over prose.
- **Define once.** Every term is defined once in the glossary; use the acronym after.
- **Consistent names.** State, prompt, and config names match across all docs.
- **Mermaid diagrams** so they render on GitHub.

## Scope

Core + CLI. The Core is the real product; the CLI is a **reference driver**. The Tauri
v2 UX is the intended human frontend — it reuses the same Core (see
[frontend.md](frontend.md)) and replaces the CLI. UX windowing details are referenced,
not specified, here.
