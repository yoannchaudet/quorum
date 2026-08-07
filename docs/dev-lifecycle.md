# Development Lifecycle

A `Makefile` defines the dev gates. Two are required: a fast `verify` and a slower
`verify-full`.

## Targets

| Target | Speed | Command | Use |
|--------|-------|---------|-----|
| `verify` | fast | `cargo test --locked --workspace --lib --bins` | Inner loop. Unit tests only; no integration, network, or `copilot`/GitHub. |
| `verify-full` | slow | `verify` + `cargo test --locked --workspace --test '*'` | Pre-merge. Adds integration tests (may exercise persistence, config, end-to-end flows). |
| `fmt` | fast | `cargo fmt --all --check` | Formatting gate. |
| `clippy` | fast | `cargo clippy --workspace --all-targets -- -D warnings` | Lint gate. |
| `build` | med | `cargo build --locked --workspace` | Compile check. |

## Sketch

```makefile
.PHONY: verify verify-full fmt clippy build

verify: fmt clippy
	cargo test --locked --workspace --lib --bins

verify-full: verify build
	cargo test --locked --workspace --test '*'

fmt:
	cargo fmt --all --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

build:
	cargo build --locked --workspace
```

## Principles

- `verify` MUST stay fast and hermetic — no network, no external CLIs. It is the inner loop.
- `verify-full` is the pre-merge gate; integration tests that touch the filesystem or mock
  external tools live here.
- `--locked` everywhere so builds are reproducible.
