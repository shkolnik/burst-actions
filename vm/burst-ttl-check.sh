#!/usr/bin/env bash
# Installed by provision.sh.tmpl into /opt/burst/burst-ttl-check.sh; fired
# every 5 min by burst-ttl.timer. Powers off once uptime exceeds the TTL.
#
# TTL_HOURS normally comes from /etc/burst/launch.env (written by launch
# user-data). The default below is the backstop for a VM whose user-data
# never ran: any burst VM that old is over budget by definition.
set -u

TTL_HOURS=6
# shellcheck disable=SC1091  # runtime file, not available to the linter
[ -f /etc/burst/launch.env ] && . /etc/burst/launch.env

uptime_sec=$(cut -d. -f1 /proc/uptime)
if [ "$uptime_sec" -ge $((TTL_HOURS * 3600)) ]; then
  systemctl poweroff
fi
exit 0
