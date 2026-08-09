#!/usr/bin/env bash
# Reduce a GitHub run's jobs JSON (stdin) to the phase-4 latency/cost lines.
# Usage: gh api repos/O/R/actions/runs/ID/jobs | bench/report.sh 0.357
set -euo pipefail
price=${1:?usage: report.sh PRICE_PER_HOUR < jobs.json}
jq -r --arg price "$price" '
  def t: fromdateiso8601;
  .jobs
  # the mode not dispatched still appears as a skipped job — drop it
  | map(select(.conclusion != "skipped"))
  | map({name, c: (.created_at|t), s: (.started_at|t), e: (.completed_at|t)})
  | (map("job=\(.name) queued=\(.s - .c) run=\(.e - .s)") | .[]),
    "total wall=\((map(.e)|max) - (map(.c)|min)) busy_instance_seconds=\(map(.e - .s)|add) cost_usd=\((map(.e - .s)|add) * (($price|tonumber) / 3600) * 10000 | round / 10000)"
'
