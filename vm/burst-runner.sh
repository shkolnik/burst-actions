#!/usr/bin/env bash
# Installed by provision.sh.tmpl into /opt/burst/burst-runner.sh.
# Runs the JIT-configured GitHub Actions runner for exactly one job, then
# powers off. instance-initiated-shutdown-behavior=terminate turns that
# poweroff into instance termination — no CLI or watcher needs to be alive.
set -euo pipefail

mkdir -p /run/burst
rm -f /run/burst/registered /run/burst/job-started

JITCONFIG="$(cat /etc/burst/jitconfig)"

# Idle timeout and requested root size from launch user-data (written before
# this service starts); the defaults are the backstop for an env file that
# never appeared. VOLUME_GB=0 means "unknown" — skip the capacity check.
IDLE_TIMEOUT_MIN=10
VOLUME_GB=0
# shellcheck disable=SC1091  # runtime file, not available to the linter
[ -f /etc/burst/launch.env ] && . /etc/burst/launch.env

# Disk contract (docs/runner-contract.md): this VM's single root volume was
# launched at VOLUME_GB and expanded at boot by cloud-init growpart/resizefs.
# Check before claiming a job — otherwise a shortfall surfaces hours later as
# ENOSPC mid-build. The observed size goes to the serial console either way,
# so `aws ec2 get-console-output` answers "what disk did it get?" without SSH.
# `|| true` on every console write: a VM with no writable /dev/console must
# still run its job.
say() { echo "burst: $1"; { echo "burst: $1" > /dev/console; } 2>/dev/null || true; }
root_gb=$(df -BG --output=size / | tail -1 | tr -dc '0-9')
say "root filesystem ${root_gb}G (requested ${VOLUME_GB}G)"
# 10% slack: filesystem overhead and GiB-vs-GB make df legitimately short.
if [ "$VOLUME_GB" -gt 0 ] && [ "$root_gb" -lt $((VOLUME_GB * 9 / 10)) ]; then
  say "root filesystem ${root_gb}G is short of the requested ${VOLUME_GB}G — refusing to claim a job"
  systemctl poweroff
  exit 1
fi

# Background watchdog: if the runner never picks up a job within the idle
# timeout, power off rather than bill for an idle instance.
(
  sleep "${IDLE_TIMEOUT_MIN}"m
  if [ ! -f /run/burst/job-started ]; then
    systemctl poweroff
  fi
) &

cd /opt/actions-runner
# `|| true`: a nonzero run.sh exit (bad JIT config, agent crash) must not
# let set -e skip the poweroff below — a failed runner still means this
# VM is done.
runuser -u burst -- ./run.sh --jitconfig "$JITCONFIG" --disableupdate 2>&1 | while IFS= read -r line; do
  echo "$line"
  case "$line" in
    *"Listening for Jobs"*)
      touch /run/burst/registered
      ;;
    *"Running job"*)
      touch /run/burst/job-started
      ;;
  esac
done || true

# run.sh exited: the one job is done (or the runner errored out). Either way
# this VM's job is finished — power off.
systemctl poweroff
