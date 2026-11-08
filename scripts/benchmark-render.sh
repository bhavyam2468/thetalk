#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
mkdir -p logs
printf 'Commit: %s\nHost: %s\n' "$(git rev-parse --short HEAD)" "$(uname -srmo)" >&2
rustc --version >&2
cargo build --release --locked
./target/release/thetalk --benchmark-render | tee logs/render-benchmark.jsonl
printf '\nCPU render timings only; not compositor FPS. Compare repeated runs on the same host.\n' >&2
