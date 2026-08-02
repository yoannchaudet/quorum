.PHONY: install check test rust-check build dev tauri-build

install:
	npm ci

check:
	npm run check

test:
	npm run test:unit
	cargo test --locked

rust-check:
	cargo fmt --all -- --check
	cargo clippy --locked --all-targets -- -D warnings

build:
	npm run build

dev:
	npm run tauri dev

tauri-build:
	npm run tauri build -- --no-bundle
