#!/usr/bin/env bash
# Real-chromium benchmark driver for the burst-bench workflow. Loops
# ~30-60s chromium chunks over workload.html until TARGET_SECONDS of
# wall-clock have elapsed. Fails loud: any chromium exit without
# CHUNK_DONE aborts the job.
set -euo pipefail

target_seconds=${1:?usage: run-bench.sh TARGET_SECONDS SEED}
seed=${2:?usage: run-bench.sh TARGET_SECONDS SEED}
here=$(cd "$(dirname "$0")" && pwd)
start=$(date +%s)
chunks=0

while :; do
  elapsed=$(( $(date +%s) - start ))
  [ "$elapsed" -ge "$target_seconds" ] && break
  out=$(mktemp)
  # --enable-logging=stderr surfaces console.log; 5-min timeout bounds a
  # hung renderer well below the job/TTL layers.
  timeout 300 xvfb-run -a chromium --no-sandbox --disable-gpu \
    --enable-logging=stderr --user-data-dir="$(mktemp -d)" \
    --headless=new "file://$here/workload.html?iters=200&seed=$seed" \
    >"$out" 2>&1 || { echo "chromium chunk failed:"; tail -20 "$out"; exit 1; }
  grep -q "CHUNK_DONE seed=$seed" "$out" \
    || { echo "chunk produced no CHUNK_DONE:"; tail -20 "$out"; exit 1; }
  rm -f "$out"
  chunks=$((chunks + 1))
done

echo "BENCH_DONE seed=$seed chunks=$chunks elapsed=$(( $(date +%s) - start ))"
