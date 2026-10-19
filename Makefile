.PHONY: test fmt clippy lint build build-release run clean ci package package-dev dev fmt-check addon addon-install

# ── Development ─────────────────────────────────────────────────────

test:
	cargo test --workspace

build:
	cargo build --workspace

build-release:
	cargo build --workspace --release

run:
	cargo run -p rust-ime-app

clean:
	cargo clean

# ── fcitx5 addon ────────────────────────────────────────────────────

# Build the C++ addon (needs libfcitx5core-dev + g++).
addon:
	$(MAKE) -C crates/fcitx5-addon

# Install system-wide (run with sudo).
addon-install: addon
	sudo ./scripts/install-fcitx5-addon.sh

# ── Quality gates ───────────────────────────────────────────────────

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

fmt-check:
	cargo fmt --all -- --check

# Full lint: format check + clippy
lint: fmt-check clippy

# ── CI parity ───────────────────────────────────────────────────────

# Mirrors .github/workflows/ci.yml for local pre-push verification.
ci: test lint build-release

# ── Package ─────────────────────────────────────────────────────────

# Build the Tauri bundle (AppImage / deb).
package:
	cargo tauri build

# Quick dev-mode Tauri build (debug, no bundle).
package-dev:
	cargo tauri build --debug

# ── Convenience ─────────────────────────────────────────────────────

# One-shot: format + test + clippy (fast iteration loop).
dev: fmt test clippy
