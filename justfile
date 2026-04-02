# Default: list available tasks
default:
    @just --list

# Setup dev environment
setup:
    cargo fetch
    cargo build
    @echo "Ready. Run 'just run' to launch kyoku."

# Build the project
build:
    cargo build

# Build release binary
release:
    cargo build --release

# Run kyoku TUI (default, no args)
run *ARGS:
    cargo run -- {{ARGS}}

# Run all tests
test:
    cargo test

# Run tests with output visible
test-verbose:
    cargo test -- --nocapture

# Lint with clippy
lint:
    cargo clippy --all-targets -- -W clippy::all

# Format code
fmt:
    cargo fmt

# Check formatting without modifying
fmt-check:
    cargo fmt --check

# Run all checks (lint + format + test)
check: fmt-check lint test

# Run with debug logging
debug *ARGS:
    RUST_LOG=debug cargo run -- {{ARGS}}

# Clean build artifacts
clean:
    cargo clean
