# Quorum development

- Run `npm ci` in a fresh checkout or worktree before running JavaScript tests, checks, or builds.
- Use `make test` for the unit test suite and `make check` for Svelte diagnostics.
- Use `make rust-check` for Rust formatting and Clippy.
- Run `make build` after frontend changes.
- Do not reset, clean, or modify a user's primary checkout when working from a managed worktree.
