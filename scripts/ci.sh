#!/usr/bin/env bash
# rsTMOG CI gate: fmt, clippy, tests, headless smoke of all pages.
# cargo-audit/deny are optional extras if installed (they are on llm).
set -eu
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build
./target/debug/rstmog --self-test
./target/debug/rstmog --smoke
if command -v cargo-audit >/dev/null; then cargo audit || echo "WARN: cargo-audit reported issues"; fi
if command -v cargo-deny >/dev/null; then cargo deny check licenses sources; fi
echo "CI PASSED"
