#!/usr/bin/env bash
#
# DinD entrypoint. Starts dockerd in the background (so the inner Docker
# daemon is available for temps deployments), waits for the socket, then
# execs whatever command compose passed.
#
# When the container's role is `noop` (just sit there), we tail
# /dev/null so the container stays alive — useful when a worker hasn't
# been seeded yet.

set -euo pipefail

start_dockerd() {
  if pgrep -x dockerd >/dev/null 2>&1; then
    return
  fi

  # Clean up stale state from previous container runs. Even though the
  # container's writable layer is recreated on every restart, the
  # /var/lib/docker volume is persistent — and on an unclean shutdown
  # dockerd can leave its pidfile + socket behind. Without this, dockerd
  # bails with:
  #   "ensure docker is not running or delete /var/run/docker.pid:
  #    process with PID N is still running"
  # because PID N happens to belong to our entrypoint shell, not a
  # zombie dockerd. Removing the stale files at boot is safe because
  # `pgrep -x dockerd` above already confirmed nothing is running.
  rm -f /var/run/docker.pid /run/docker.pid /var/run/docker.sock

  # cgroup v2 nesting fix.
  #
  # Docker Desktop runs the host on cgroup v2. When our worker
  # container starts, the kernel gives us a v2 cgroup at
  # /sys/fs/cgroup with `cpuset cpu pids` enabled in subtree_control.
  # Without further setup, the inner dockerd's child cgroups inherit
  # only those controllers, so runc later fails with:
  #   "cannot enter cgroupv2 /sys/fs/cgroup/docker with domain
  #    controllers -- it is in an invalid state"
  # whenever a container with memory/io limits is started.
  #
  # The kernel forbids enabling additional controllers in
  # subtree_control while there are processes in the cgroup. So we
  # must:
  #   1. Move every PID currently in / into a sub-cgroup (/init).
  #   2. THEN enable the controllers.
  # After that the root has no procs and the kernel lets us turn on
  # memory/io/hugetlb. dockerd inherits these, all child cgroups
  # work, runc is happy.
  #
  # This is the canonical docker:dind setup, ported here because we
  # use a custom debian-based DinD image rather than docker:dind.
  if [[ -f /sys/fs/cgroup/cgroup.controllers ]]; then
    if ! mkdir -p /sys/fs/cgroup/init 2>/dev/null; then
      echo "[entrypoint] WARN: cannot create /sys/fs/cgroup/init — cgroup v2 nesting may fail" >&2
    else
      # Move every process out of the root cgroup
      while read -r pid; do
        echo "$pid" > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
      done < /sys/fs/cgroup/cgroup.procs
      # Now enable every available controller in the root's subtree_control.
      # +ctrl tokens go ONE PER WRITE; a single multi-token write fails on
      # some kernels.
      for ctrl in $(cat /sys/fs/cgroup/cgroup.controllers); do
        echo "+$ctrl" > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null || true
      done
      echo "[entrypoint] cgroup v2 nested-controllers enabled: $(cat /sys/fs/cgroup/cgroup.subtree_control)"
    fi
  fi

  # We let dockerd create its default docker0 bridge — BuildKit needs
  # *some* network for image-build RUN steps, and refuses to run with
  # `--bridge=none` (every RUN dies with "network bridge not found").
  # Docker's default docker0 lives on 172.17.0.0/16, which doesn't
  # collide with our compute pool (172.20.0.0/16). The temps-network
  # crate creates `br-temps0` as a separate bridge anyway, so coexisting
  # with docker0 is fine.
  # --pidfile pinned so we know exactly which file to clean on restart.
  dockerd \
    --host=unix:///var/run/docker.sock \
    --pidfile=/var/run/docker.pid \
    --iptables=true \
    --log-level=warn \
    >/var/log/docker.log 2>&1 &

  # Containerd takes 5–30s to come up on a cold worker volume; up to
  # 60s when several containers race to start at once on a constrained
  # host (Docker Desktop's Linux VM gets thrashed). 90s is the
  # observed safe upper bound. Keep retrying with brief logs so the
  # operator sees what's happening rather than a silent freeze.
  for i in $(seq 1 90); do
    if docker info >/dev/null 2>&1; then
      echo "[entrypoint] dockerd ready after ${i}s"
      return
    fi
    if (( i % 10 == 0 )); then
      echo "[entrypoint] still waiting for dockerd (${i}s) — last log:"
      tail -n 1 /var/log/docker.log >&2 || true
    fi
    sleep 1
  done
  echo "[entrypoint] dockerd failed to start; tail of /var/log/docker.log:" >&2
  tail -n 80 /var/log/docker.log >&2 || true
  exit 1
}

start_dockerd

# Honour any args. Default `bash` keeps the container alive interactively.
exec "$@"
