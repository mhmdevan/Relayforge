#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT_DIR="${ROOT_DIR}/artifacts/perf"
OUT_LOG="${OUT_DIR}/bench-webhook-hot-path.log"
ITERATIONS="${ITERATIONS:-200000}"

mkdir -p "${OUT_DIR}"

cd "${ROOT_DIR}"

cargo run -p api --release --bin webhook_hot_path_bench -- "${ITERATIONS}" | tee "${OUT_LOG}"

echo "Bench log written to ${OUT_LOG}"
