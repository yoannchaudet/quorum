# Quorum Documentation

Specifications for Quorum's **Core** and **CLI**. Design-on-paper; no code yet.

## Reading order

1. [glossary.md](glossary.md) — terms and acronyms (read first).
2. [architecture.md](architecture.md) — Core vs CLI, Rust workspace, future Tauri.
3. [work-items.md](work-items.md) — what a WI is; GitHub pull; images.
4. [repositories.md](repositories.md) — repository context and registration.
5. [state-machine.md](state-machine.md) — the backbone: states, loops, HI signaling.
6. [agents.md](agents.md) — CO, PL quorum, IM↔RV, convergence, HI.
7. [isolation.md](isolation.md) — execution isolation: local sandbox + per-role profiles.
8. [prompts.md](prompts.md) — prompts as markdown files with frontmatter.
9. [sessions.md](sessions.md) — HI via resumable `copilot` sessions.
10. [config.md](config.md) — `~/.quorum/config.yaml` schema.
11. [persistence.md](persistence.md) — crash resilience and recovery.
12. [dev-lifecycle.md](dev-lifecycle.md) — Makefile: `verify` / `verify-full`.

## Doc style rules

- **Non-verbose.** Straight to the point. Prefer tables and diagrams over prose.
- **Define once.** Every term is defined once in the glossary; use the acronym after.
- **Consistent names.** State, prompt, and config names match across all docs.
- **Mermaid diagrams** so they render on GitHub.

## Scope

Core + CLI only. The Tauri v2 UX is future sugar over the Core and is referenced, not
specified, here.
