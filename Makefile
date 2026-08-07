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
