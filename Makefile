.PHONY: verify verify-full fmt clippy build test-fast test-slow

test-fast:
	cd app && npm run test
	cargo test --workspace --lib --bins

test-slow: verify build
	cargo test --locked --workspace --test '*'

verify: fmt clippy test-fast

fmt:
	cargo fmt --all --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

build:
	cargo build --locked --workspace
