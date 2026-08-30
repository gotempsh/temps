#!/bin/sh
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -eu

: "${TEMPS_CONTROL_INTERNAL_IP:?TEMPS_CONTROL_INTERNAL_IP must be set}"
: "${TEMPS_INGRESS_MODE:?TEMPS_INGRESS_MODE must be public or admin}"
: "${TEMPS_INGRESS_LISTEN_IP:?TEMPS_INGRESS_LISTEN_IP must be set}"

write_common_configuration() {
  cat > /tmp/nginx.conf <<'EOF'
load_module /usr/lib/nginx/modules/ngx_stream_module.so;

pid /tmp/nginx.pid;
error_log /dev/stderr warn;
worker_processes 1;

events {
  worker_connections 512;
}
EOF
}

write_public_configuration() {
  cat >> /tmp/nginx.conf <<EOF

stream {
  # Public traffic is reachable from managed workloads by design. Bound
  # concurrent connections without treating a quiet WebSocket, SSE, or TLS
  # connection as abandoned after only a few seconds.
  limit_conn_zone \$binary_remote_addr zone=public_clients:1m;
  limit_conn_log_level warn;
  proxy_connect_timeout 3s;
  proxy_timeout 1h;

  server {
    listen ${TEMPS_INGRESS_LISTEN_IP}:3000;
    limit_conn public_clients 64;
    proxy_pass ${TEMPS_CONTROL_INTERNAL_IP}:3000;
  }

  server {
    listen ${TEMPS_INGRESS_LISTEN_IP}:3443;
    limit_conn public_clients 64;
    proxy_pass ${TEMPS_CONTROL_INTERNAL_IP}:3443;
  }

  server {
    listen ${TEMPS_INGRESS_LISTEN_IP}:9000;
    limit_conn public_clients 64;
    proxy_pass ${TEMPS_CONTROL_INTERNAL_IP}:9000;
  }
}
EOF
}

write_admin_configuration() {
  admin_upstream_port=${TEMPS_ADMIN_UPSTREAM_PORT:-9001}
  admin_password_file=/run/secrets/temps_admin_ingress_password
  if [ ! -r "$admin_password_file" ]; then
    echo "Temps admin ingress password secret is not readable" >&2
    exit 1
  fi

  mkdir -p /tmp/nginx/client_body /tmp/nginx/proxy /tmp/nginx/fastcgi \
    /tmp/nginx/uwsgi /tmp/nginx/scgi

  # Read the independent ingress password from stdin so it is never present in
  # the process arguments or environment. htpasswd writes only the bcrypt hash.
  htpasswd -nBi temps < "$admin_password_file" > /tmp/admin.htpasswd

  cat >> /tmp/nginx.conf <<EOF

http {
  access_log /dev/stdout combined;
  client_body_temp_path /tmp/nginx/client_body;
  proxy_temp_path /tmp/nginx/proxy;
  fastcgi_temp_path /tmp/nginx/fastcgi;
  uwsgi_temp_path /tmp/nginx/uwsgi;
  scgi_temp_path /tmp/nginx/scgi;

  limit_req_zone \$binary_remote_addr zone=admin_auth:1m rate=10r/s;
  map \$http_upgrade \$connection_upgrade {
    default upgrade;
    '' close;
  }

  server {
    listen ${TEMPS_INGRESS_LISTEN_IP}:9001;
    auth_delay 1s;

    location / {
      limit_req zone=admin_auth burst=20 nodelay;
      auth_basic "Temps admin";
      auth_basic_user_file /tmp/admin.htpasswd;

      # Basic auth is an independent ingress barrier. Never pass its
      # Authorization header into the Temps application.
      proxy_http_version 1.1;
      proxy_set_header Authorization "";
      proxy_set_header Host \$host;
      proxy_set_header X-Forwarded-For \$remote_addr;
      proxy_set_header X-Forwarded-Proto \$scheme;
      proxy_set_header Upgrade \$http_upgrade;
      proxy_set_header Connection \$connection_upgrade;
      proxy_buffering off;
      proxy_read_timeout 1h;
      proxy_send_timeout 1h;
      proxy_pass http://${TEMPS_CONTROL_INTERNAL_IP}:${admin_upstream_port};
    }
  }
}
EOF
}

write_common_configuration
case "$TEMPS_INGRESS_MODE" in
  public)
    write_public_configuration
    ;;
  admin)
    write_admin_configuration
    ;;
  *)
    echo "Unsupported TEMPS_INGRESS_MODE '$TEMPS_INGRESS_MODE'; expected public or admin" >&2
    exit 1
    ;;
esac

exec nginx -c /tmp/nginx.conf -g 'daemon off;'
