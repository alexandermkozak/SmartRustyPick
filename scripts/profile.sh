#!/usr/bin/env bash
# Profile the engine benchmarks and produce a flamegraph.
#
# The Criterion benchmarks in crates/core/benches are the right profiling target: they
# are release-mode, deterministic, and exercise the same code paths the performance
# suites drive over the wire, without the TLS and JSON noise in between.
#
# Usage: scripts/profile.sh [criterion-filter]
#   scripts/profile.sh                     # profile every benchmark
#   scripts/profile.sh query/              # only the query benchmarks
#   scripts/profile.sh storage/build_and_save
set -euo pipefail

cd "$(dirname "$0")/.."

FILTER="${1:-}"
OUT_DIR="target/profile"
mkdir -p "$OUT_DIR"

# --profile-time makes Criterion run the benchmark for a fixed wall time without its
# statistical analysis, which is what a sampling profiler wants.
BENCH_ARGS=(--bench --profile-time 10)
[ -n "$FILTER" ] && BENCH_ARGS+=("$FILTER")

if command -v cargo-flamegraph >/dev/null 2>&1 || cargo flamegraph --version >/dev/null 2>&1; then
    echo "Profiling with cargo-flamegraph..."
    cargo flamegraph --output "$OUT_DIR/flamegraph.svg" -p smart-rusty-pick-core \
        --bench query -- "${BENCH_ARGS[@]}"
    echo "Wrote $OUT_DIR/flamegraph.svg"
    exit 0
fi

if command -v perf >/dev/null 2>&1; then
    echo "cargo-flamegraph not found; recording with perf instead..."
    cargo build --release --benches -p smart-rusty-pick-core
    BIN=$(ls -t target/release/deps/query-* | grep -v '\.d$' | head -1)
    perf record -g -o "$OUT_DIR/perf.data" "$BIN" "${BENCH_ARGS[@]}"
    perf report -i "$OUT_DIR/perf.data" --stdio | head -60
    echo "Full profile in $OUT_DIR/perf.data (perf report -i $OUT_DIR/perf.data)"
    exit 0
fi

cat >&2 <<'EOF'
No profiler found. Install one of:

  cargo install flamegraph      # then: scripts/profile.sh
  sudo apt install linux-tools-common linux-tools-generic   # provides perf

Without a profiler you can still compare costs between revisions:

  cargo bench -p smart-rusty-pick-core          # Criterion reports per-bench timings
  make perf-compare BASE=/tmp/base.json         # end-to-end metrics diff
EOF
exit 1
