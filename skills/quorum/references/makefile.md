# The Makefile contract

Quorum requires exactly two entry points from any repository it works in. Everything else
about the build is the repository's business.

| Target | Speed | Contains | Used |
|--------|-------|----------|------|
| `verify` | fast | format check, lint, unit tests | Before **every** review round — the inner loop |
| `verify-full` | slow | `verify` + build + integration / e2e tests | Once, after the reviewer accepts |

Principles:

- `verify` must stay **fast and hermetic**. No network, no external CLIs, no services. If
  it takes minutes, the inner loop stops being an inner loop and the reviewer gets stale
  work.
- `verify-full` is the pre-pull-request gate. Anything touching the filesystem, real
  services, browsers, or mocked external tools lives here.
- Both must be **deterministic and locked**. Pin dependencies so a green run means
  something.
- Both must exit non-zero on failure. The loop reads exit codes.

## Checking the contract

At the start of a Quorum run:

```bash
make -n verify >/dev/null 2>&1 && echo "verify: ok" || echo "verify: MISSING"
make -n verify-full >/dev/null 2>&1 && echo "verify-full: ok" || echo "verify-full: MISSING"
```

If both exist, use them as-is — do not rewrite a working Makefile. If a repo has the
right idea under different names (`make test` / `make ci`), add thin `verify` and
`verify-full` aliases rather than restructuring.

## Bootstrapping

If either target is missing, detect the ecosystem, draft a Makefile from the matching
template below, show it to the human, and get an explicit OK before proceeding. Do not
start planning against a contract the human has not agreed to.

Detection: `Cargo.toml` → Rust, `package.json` → Node, `pyproject.toml` /
`requirements.txt` → Python, `go.mod` → Go. In a polyglot repo, compose the targets from
every ecosystem present.

### Rust

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

### Node / TypeScript

```makefile
.PHONY: verify verify-full lint typecheck build

verify: lint typecheck
	npm run test -- --run

verify-full: verify build
	npm run test:e2e

lint:
	npm run lint

typecheck:
	npx tsc --noEmit

build:
	npm run build
```

### Python

```makefile
.PHONY: verify verify-full lint typecheck

verify: lint typecheck
	pytest -q -m "not integration"

verify-full: verify
	pytest -q

lint:
	ruff check .
	ruff format --check .

typecheck:
	mypy .
```

### Go

```makefile
.PHONY: verify verify-full vet build

verify: vet
	go test ./... -short

verify-full: verify build
	go test ./... -race

vet:
	go vet ./...

build:
	go build ./...
```

## When a target does not apply

If a repo genuinely has no integration tests, `verify-full` should still exist and should
still do *more* than `verify` — at minimum a full build and a non-short test run. Do not
alias `verify-full` to `verify`; that silently deletes the slow gate.

If a repo has no tests at all, say so to the human before planning. The adversarial loop
leans on the fast gate, and without one the reviewer carries the entire burden.
