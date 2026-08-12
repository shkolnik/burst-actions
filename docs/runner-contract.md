# Runner contract

What a job running on a burst runner can rely on, and what it must not. Written for the agent or
maintainer wiring a repo up to `burst`; the tool's own design lives in `design-proposal.md`.

## Shape

One VM per job, one job per VM. The VM is launched from a prebaked AMI, registers with a
single-use JIT config, runs **exactly one job**, and terminates itself. Nothing is shared between
jobs and nothing survives them: no layer cache, no dataset cache, no workspace.

## Disk

**One volume.** The VM has a single gp3 root volume. The runner workspace and the container data
root (`/var/lib/docker`) are both on it — there is no second disk to point anything at.

**Size is yours to choose**, in the consuming repo's `burst.toml`:

```toml
[burst]
volume_gb = 750               # default 100
volume_iops = 6000            # optional; gp3 baseline is 3000
volume_throughput_mbps = 1000 # optional; gp3 baseline is 125 MB/s
```

gp3's limits are checked when the config loads, not at launch: 1–16384 GiB, 3000–16000 IOPS capped
at 500 IOPS/GiB, 125–1000 MB/s capped at 0.25 MB/s per provisioned IOPS (so 1000 MB/s requires
`volume_iops` ≥ 4000). A violation names the key and the ceiling.

`volume_iops` and `volume_throughput_mbps` cost extra per volume-hour and are worth setting for a
write-heavy job: at the 125 MB/s baseline, writing 500 GiB takes over an hour of pure I/O time,
which reads as "the job is slow", not as "the volume is throttled".

**The guarantee.** `burst up` sets the root volume's size on `RunInstances`, so EC2 either creates
a volume of exactly `volume_gb` or fails the launch. The base images' cloud-init expands the
partition and filesystem at boot (`growpart`/`resizefs`, before user-data runs). Before the runner
claims a job it checks `df /` against the requested size and, if it is more than 10% short,
powers the VM off rather than accepting work it cannot finish. Either way it prints

```
burst: root filesystem 737G (requested 750G)
```

to the serial console, so `aws ec2 get-console-output --instance-id i-...` answers "what disk did
it get?" without SSH.

`df` legitimately reports a few percent under `volume_gb`: filesystem overhead, plus GiB-vs-GB.
Size for the peak you measured plus headroom, not to the last gigabyte.

**If you assert it yourself**, assert on the filesystem holding `$GITHUB_WORKSPACE` and
`/var/lib/docker` — the same one. A shortfall larger than the 10% slack is a burst bug; report it
with the console line above.

## Timeouts

Two independent timers, both baked into the AMI and armed at boot — neither depends on the `burst`
CLI still running.

- **`idle_timeout_min`** (default 10) applies **only before a job is claimed**. If the runner has
  not picked up a job within the window, the VM powers off. Once a job starts, this timer is
  irrelevant no matter how quiet the job is — a three-hour single `curl` is not "idle".
- **`ttl_hours`** (default 6) is a hard cap on VM *uptime*, checked every 5 minutes, unconditional
  on what the VM is doing. It is the backstop that bounds worst-case billing when everything else
  fails.

**Set `ttl_hours` above your longest job's `timeout-minutes` plus a few minutes of boot.** A TTL
kill mid-job terminates the VM under the running job. GitHub then sees a runner that vanished
without unregistering; it fails the job rather than completing it, and `burst sweep` tidies the
orphaned registration. *(Verified by us: the VM dies. Not verified by us: how quickly GitHub gives
up on the vanished runner and fails the job — plan as if it could take until the job's own
`timeout-minutes`.)*

## Credentials

The VM receives exactly one credential: its single-use JIT runner config. The GitHub PAT that
`burst` runs with never reaches a VM, and there is nothing on the VM to authenticate to AWS with
beyond the instance's own scoped role. Anything a job needs — registry logins, cloud credentials —
comes from the workflow's own secrets, as on any runner.

## The `provision` key

```toml
[burst]
provision = ".burst/provision.sh"   # relative to burst.toml's directory
```

The named script is appended to burst's own provisioning script and runs **at bake time, as root,
on the builder VM** — after the base packages, the `burst` user, and the runner agent are in
place, so it can reference all of them. It is not run per job.

- **Its bytes are part of the image cache key.** Editing the script changes the key, so the next
  `burst bake`/`burst up` rebuilds the AMI instead of silently reusing a stale one.
- **It is baked, not per-job.** It cannot see the job, the repo checkout, or per-launch config.
- **Anything it installs lands on the root volume**, which is the volume `volume_gb` sizes — so a
  container daemon installed here needs no data-root relocation.
- **Failure fails the bake**, loudly, before any fleet launches.

### Docker on the Debian 13 base

Measured in a `debian:trixie` container (not on a baked AMI): `docker.io` 26.1.5 alone is not
enough. `docker build` is aliased to `docker buildx build` **only when the buildx plugin is
present**, and `docker compose` is a separate package:

```sh
apt-get install -y docker.io docker-buildx docker-compose
usermod -aG docker burst
```

With `docker-buildx` (0.13.1) installed, `docker build --help` reports usage as
`docker buildx build` and accepts `--build-context`; `docker compose version` reports Compose
2.26.1 from `docker-compose` (that package name is Compose v2 on trixie — there is no
`docker-compose-v2`). Without those two plugins both failures land at the first job, a full bake
cycle away from the mistake.
