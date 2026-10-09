#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Serve a console bundle built from this checkout in front of a running
# `temps serve` whose embedded console is a different one.
#
# Why: on a pull request that changes no Rust input, rust-tests.yml reuses the
# binary `main` built instead of compiling one (find-reusable-binary.py). That
# binary embeds main's console, so the browser suite would pass while testing
# none of the pull request's changes. This puts nginx on the console port: it
# serves the pull request's bundle and forwards everything the binary answers
# itself, so the browser sees one origin exactly as it does without it.
#
# The split mirrors crates/temps-cli/src/commands/serve/console.rs:
#   /api, /api/*        the admin and public API routers (incl. WebSockets/SSE)
#   /mcp, /mcp/*        root-level MCP routes (ADR-039)
#   /healthz, /readyz   health probes
#   anything else       `serve_static_from`: the file if the bundle has it,
#                       otherwise index.html, with the same Cache-Control
#
# Requests reach the binary from 127.0.0.1 with the browser's own Host header
# and no X-Forwarded-* headers, which is what a direct connection from the
# runner looked like, so nothing that keys on the peer or the origin changes.
#
# Usage: serve-console-from-source.sh <dist-dir> <listen-port> <upstream-port> [work-dir]

set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 ]]; then
  echo "usage: $0 <dist-dir> <listen-port> <upstream-port> [work-dir]" >&2
  exit 2
fi

DIST=$(cd "$1" && pwd)
LISTEN_PORT=$2
UPSTREAM_PORT=$3
WORK=${4:-/tmp/temps-console-proxy}

for port in "$LISTEN_PORT" "$UPSTREAM_PORT"; do
  if ! [[ "$port" =~ ^[0-9]+$ ]]; then
    echo "error: port '$port' is not a number" >&2
    exit 2
  fi
done
if [[ ! -f "$DIST/index.html" ]]; then
  echo "error: $DIST/index.html does not exist; build the console first (cd web && bun run build)" >&2
  exit 1
fi

if ! command -v nginx >/dev/null 2>&1; then
  echo "nginx is not installed; installing it"
  sudo apt-get update -qq
  sudo apt-get install -y -qq --no-install-recommends nginx >/dev/null
  # The package starts a system nginx on port 80; it is not the one we use.
  sudo systemctl stop nginx 2>/dev/null || true
fi
NGINX=$(command -v nginx)

MIME_TYPES=""
for candidate in /etc/nginx/mime.types /opt/homebrew/etc/nginx/mime.types /usr/local/etc/nginx/mime.types; do
  if [[ -f "$candidate" ]]; then
    MIME_TYPES=$candidate
    break
  fi
done
if [[ -z "$MIME_TYPES" ]]; then
  echo "error: no nginx mime.types found; JavaScript would be served as application/octet-stream" >&2
  exit 1
fi

mkdir -p "$WORK"

cat > "$WORK/proxy.conf" <<EOF
proxy_pass http://127.0.0.1:${UPSTREAM_PORT};
proxy_http_version 1.1;
proxy_set_header Host \$http_host;
proxy_set_header Upgrade \$http_upgrade;
proxy_set_header Connection \$connection_upgrade;
# Log streams, SSE and uploads must flow through, not be held in a buffer.
proxy_buffering off;
proxy_request_buffering off;
proxy_read_timeout 3600s;
proxy_send_timeout 3600s;
proxy_redirect off;
EOF

# Started as root, nginx drops its workers to `nobody`, which cannot read a
# bundle in a private temp directory. Keep them as the invoking user.
USER_DIRECTIVE=""
if [[ $(id -u) -eq 0 ]]; then
  USER_DIRECTIVE="user root;"
fi

cat > "$WORK/nginx.conf" <<EOF
${USER_DIRECTIVE}
worker_processes 1;
pid ${WORK}/nginx.pid;
error_log ${WORK}/error.log warn;
events { worker_connections 1024; }
http {
  include ${MIME_TYPES};
  default_type application/octet-stream;
  access_log ${WORK}/access.log;
  client_body_temp_path ${WORK}/client_body;
  proxy_temp_path ${WORK}/proxy_temp;
  fastcgi_temp_path ${WORK}/fastcgi_temp;
  uwsgi_temp_path ${WORK}/uwsgi_temp;
  scgi_temp_path ${WORK}/scgi_temp;
  # The binary enforces its own upload limits; nginx's 1 MB default would
  # reject uploads the binary accepts.
  client_max_body_size 0;
  map \$http_upgrade \$connection_upgrade {
    default upgrade;
    ''      close;
  }
  server {
    listen ${LISTEN_PORT};
    root ${DIST};

    location ~ ^/(api|mcp)(/|\$) { include ${WORK}/proxy.conf; }
    location = /healthz { include ${WORK}/proxy.conf; }
    location = /readyz { include ${WORK}/proxy.conf; }

    location = /index.html {
      add_header Cache-Control "no-cache, no-store, must-revalidate" always;
    }
    location /static/ {
      try_files \$uri /index.html;
      add_header Cache-Control "public, max-age=31536000, immutable";
    }
    location /assets/ {
      try_files \$uri /index.html;
      add_header Cache-Control "public, max-age=31536000, immutable";
    }
    location / {
      try_files \$uri /index.html;
      add_header Cache-Control "public, max-age=0, must-revalidate";
    }
  }
}
EOF

"$NGINX" -t -q -p "$WORK" -e "$WORK/error.log" -c "$WORK/nginx.conf"

# From here on nginx is running. Any failed check below must not leave it
# holding the console port, so stop it on every exit until the checks pass;
# INT/TERM become an exit so the trap runs for them too. (SIGKILL cannot be
# trapped -- callers that can kill this script also stop nginx via its pid
# file, $WORK/nginx.pid.)
stop_nginx() {
  "$NGINX" -q -p "$WORK" -e "$WORK/error.log" -c "$WORK/nginx.conf" -s stop 2>/dev/null \
    || { [[ -f "$WORK/nginx.pid" ]] && kill "$(cat "$WORK/nginx.pid")" 2>/dev/null; } \
    || true
}
trap stop_nginx EXIT
trap 'exit 130' INT TERM
"$NGINX" -p "$WORK" -e "$WORK/error.log" -c "$WORK/nginx.conf"

console="http://127.0.0.1:${LISTEN_PORT}"
if ! timeout 30 bash -c "until curl -sf -o /dev/null '$console/index.html'; do sleep 1; done"; then
  echo "error: nginx did not start serving $console within 30s" >&2
  cat "$WORK/error.log" >&2 || true
  exit 1
fi

# Prove the browser will get this checkout's bundle, on the root and on a
# client-side route, and that the binary is reachable through the proxy.
for path in / /projects/this-route-only-exists-client-side; do
  if ! curl -sf "$console$path" | cmp -s - "$DIST/index.html"; then
    echo "error: $console$path did not return $DIST/index.html" >&2
    exit 1
  fi
done
if ! curl -sf -o /dev/null "$console/healthz"; then
  echo "error: $console/healthz did not reach the binary on port $UPSTREAM_PORT" >&2
  cat "$WORK/error.log" >&2 || true
  exit 1
fi

# Every check passed: leave nginx serving for the rest of the job.
trap - EXIT INT TERM
echo "Serving the console from $DIST on port $LISTEN_PORT; API, MCP and health probes go to port $UPSTREAM_PORT"
