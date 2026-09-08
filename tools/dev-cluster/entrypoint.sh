#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

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
  #
  # Pin BOTH the default bridge and the pool user-defined bridges are
  # carved from, so nothing Docker auto-allocates can ever land on the
  # cluster compute pool (172.20.0.0/16, the `network_config` default).
  #
  # It is not enough to note that docker0 defaults to 172.17.0.0/16.
  # Docker's *default* address pool is 172.17.0.0/12 with size 16, so
  # every user-defined bridge walks up 172.18, 172.19, 172.20, … — and
  # this node creates several at boot (the preview-gateway control and
  # ingress networks, then `temps-app-network`). Whichever one lands
  # fourth gets 172.20.0.0/16 and collides head-on with the compute
  # pool, which makes `temps network setup-multi-node` refuse to enable
  # the overlay:
  #   "compute pool 172.20.0.0/16 overlaps host route 172.20.0.0/16
  #    on device 'br-<hash>'"
  # Because those boot-time reconcilers race, which network gets which
  # /16 varies run to run — so the multinode e2e scenario failed only
  # intermittently until this was pinned.
  #
  # The replacement pool must avoid 172.16.0.0/12 entirely, not just
  # 172.20.0.0/16: this container's OWN eth0 is frequently on a
  # 172.x bridge belonging to the outer daemon, and Docker refuses to
  # subnet a pool that overlaps an existing route — pinning the pool to
  # 172.17.0.0/16 makes every `docker network create` fail outright with
  # "all predefined address pools have been fully subnetted".
  #
  # 10.98/10.99 is clear of the compute pool (172.20.0.0/16), of both
  # cluster underlays (dev-cluster 10.42.0.0/24, e2e-multinode
  # 10.52.0.0/24), and of the outer daemon's 172.16.0.0/12 defaults.
  # 256 /24s is far more networks than any scenario creates. Overlay
  # bridges (`br-temps0`) are created by the temps-network crate with an
  # EXPLICIT subnet out of the compute pool, so they are unaffected by
  # this pool and still land where the allocator expects.
  # --pidfile pinned so we know exactly which file to clean on restart.
  dockerd \
    --host=unix:///var/run/docker.sock \
    --pidfile=/var/run/docker.pid \
    --iptables=true \
    --bip=10.98.0.1/24 \
    --default-address-pool base=10.99.0.0/16,size=24 \
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
