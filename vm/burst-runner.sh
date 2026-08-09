#!/usr/bin/env bash
# Installed by provision.sh.tmpl into /opt/burst/burst-runner.sh.
# Runs the JIT-configured GitHub Actions runner for exactly one job, then
# powers off. instance-initiated-shutdown-behavior=terminate turns that
# poweroff into instance termination — no CLI or watcher needs to be alive.
set -euo pipefail

mkdir -p /run/burst
rm -f /run/burst/registered /run/burst/job-started

JITCONFIG="$(cat /etc/burst/jitconfig)"

# Background watchdog: if the runner never picks up a job within the idle
# timeout, power off rather than bill for an idle instance.
(
  sleep "__BURST_IDLE_TIMEOUT_MIN__"m
  if [ ! -f /run/burst/job-started ]; then
    systemctl poweroff
  fi
) &

cd /opt/actions-runner
./run.sh --jitconfig "$JITCONFIG" --disableupdate 2>&1 | while IFS= read -r line; do
  echo "$line"
  case "$line" in
    *"Listening for Jobs"*)
      touch /run/burst/registered
      ;;
    *"Running job"*)
      touch /run/burst/job-started
      ;;
  esac
done

# run.sh exited: the one job is done (or the runner errored out). Either way
# this VM's job is finished — power off.
systemctl poweroff
