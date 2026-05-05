#!/bin/sh
# sandbox-entrypoint.sh — start the PTY agent under docker-init, then hand
# off to whatever command the container was asked to run (typically
# `sleep infinity`).
#
# docker-init (PID 1, tini-like) reaps zombies and propagates SIGTERM to
# us. We background the agent so it runs alongside the main command. If
# the agent dies, we respawn it — it's the in-container owner of every
# interactive terminal, so the container becomes useless without it.
#
# We do NOT use `exec` for the main command because we need this script
# to keep supervising the agent. The overhead of one extra sh process is
# negligible; the reliability of the supervise loop is not.

set -u

SOCKET_DIR="/run/temps-pty"
SOCKET_PATH="${SOCKET_DIR}/agent.sock"
AGENT_BIN="/usr/local/bin/temps-pty-agent"
LOG_PREFIX="[sandbox-entrypoint]"

# Ensure the socket dir exists with correct ownership. The Dockerfile
# already does this, but a mounted tmpfs or a volume remount could wipe
# it — the supervisor is the safe place to re-establish.
mkdir -p "$SOCKET_DIR"
chown temps:temps "$SOCKET_DIR" 2>/dev/null || true
chmod 0700 "$SOCKET_DIR"
# Remove any stale socket from a previous container life — the agent's
# own bind() cleans this too, but doing it here avoids a log warning on
# first start.
rm -f "$SOCKET_PATH"

supervise_agent() {
    # Respawn loop: `until false` is an infinite loop with a crash-backoff
    # so a broken binary doesn't spin the CPU.
    #
    # The sandbox Dockerfile ends with `USER temps`, so when Docker invokes
    # the entrypoint we're *already* running as uid 1000. Using `su` to
    # re-enter the same user would fail without a password (and even with
    # `--session-command` it's extra PID noise). Just run the agent
    # inline. If the container were ever started as root (`--user 0`) we'd
    # fall through to `su` so the PTYs the agent spawns still land under
    # uid 1000.
    current_uid=$(id -u)
    while true; do
        if [ ! -x "$AGENT_BIN" ]; then
            echo "$LOG_PREFIX $AGENT_BIN not executable, skipping agent" >&2
            return 0
        fi
        echo "$LOG_PREFIX starting temps-pty-agent (socket=$SOCKET_PATH uid=$current_uid)" >&2
        if [ "$current_uid" = "0" ]; then
            su -s /bin/sh -c "$AGENT_BIN --socket $SOCKET_PATH" temps
        else
            "$AGENT_BIN" --socket "$SOCKET_PATH"
        fi
        rc=$?
        echo "$LOG_PREFIX temps-pty-agent exited rc=$rc; restarting in 1s" >&2
        sleep 1
    done
}

# Note: the git credential daemon is NOT supervised by this entrypoint.
# `no-new-privileges:true` is enabled on the sandbox container (per the
# bollard host config in temps-agents/src/sandbox/docker.rs), which
# blocks `sudo`/setuid uid changes. The entrypoint itself runs as uid
# 1000 (`temps`) and so cannot launch the daemon as uid 1001
# (`temps-git`) from inside the container.
#
# Instead, the daemon is launched by the host-side message_executor via
# `docker exec --user temps-git -d` when it writes the daemon's env
# file at session start. `docker exec` bypasses no-new-privileges
# because it's a Docker API call, not an in-container setuid attempt.
# See `start_credential_daemon` in temps-workspace/services/message_executor.rs.

# Background the pty-agent supervisor and run the container's command
# in the foreground so docker-init can signal it on container stop.
supervise_agent &
SUPERVISOR_PID=$!

# Forward termination to the supervisor so it doesn't linger as an orphan
# when the container stops.
trap 'kill -TERM $SUPERVISOR_PID 2>/dev/null; wait $SUPERVISOR_PID 2>/dev/null; exit 0' TERM INT

# Default command when none is given.
if [ "$#" -eq 0 ]; then
    set -- sleep infinity
fi

"$@"
