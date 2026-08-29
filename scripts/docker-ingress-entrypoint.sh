#!/bin/sh
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -eu

: "${TEMPS_CONTROL_INTERNAL_IP:?TEMPS_CONTROL_INTERNAL_IP must be set}"
: "${TEMPS_INGRESS_PUBLISH_IP:?TEMPS_INGRESS_PUBLISH_IP must be set}"

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

cat > /tmp/nginx.conf <<EOF
load_module /usr/lib/nginx/modules/ngx_stream_module.so;

pid /tmp/nginx.pid;
error_log /dev/stderr warn;
worker_processes 1;

events {
  worker_connections 512;
}

stream {
  # Public traffic is reachable from managed workloads by design. Use one
  # event-driven proxy with an aggregate per-source connection ceiling rather
  # than a process-per-connection forwarder. Close idle streams promptly.
  limit_conn_zone \$binary_remote_addr zone=public_clients:1m;
  limit_conn_log_level warn;
  proxy_connect_timeout 3s;
  proxy_timeout 10s;

  server {
    listen ${TEMPS_INGRESS_PUBLISH_IP}:3000;
    limit_conn public_clients 64;
    proxy_pass ${TEMPS_CONTROL_INTERNAL_IP}:3000;
  }

  server {
    listen ${TEMPS_INGRESS_PUBLISH_IP}:3443;
    limit_conn public_clients 64;
    proxy_pass ${TEMPS_CONTROL_INTERNAL_IP}:3443;
  }

  server {
    listen ${TEMPS_INGRESS_PUBLISH_IP}:9000;
    limit_conn public_clients 64;
    proxy_pass ${TEMPS_CONTROL_INTERNAL_IP}:9000;
  }
}

http {
  access_log /dev/stdout combined;
  client_body_temp_path /tmp/nginx/client_body;
  proxy_temp_path /tmp/nginx/proxy;
  fastcgi_temp_path /tmp/nginx/fastcgi;
  uwsgi_temp_path /tmp/nginx/uwsgi;
  scgi_temp_path /tmp/nginx/scgi;

  limit_req_zone \$binary_remote_addr zone=admin_auth:1m rate=10r/s;

  server {
    listen ${TEMPS_INGRESS_PUBLISH_IP}:9001;
    auth_delay 1s;

    location / {
      limit_req zone=admin_auth burst=20 nodelay;
      auth_basic "Temps admin";
      auth_basic_user_file /tmp/admin.htpasswd;

      # Basic auth is an independent ingress barrier. Never pass its
      # Authorization header into the Temps application.
      proxy_set_header Authorization "";
      proxy_set_header Host \$host;
      proxy_set_header X-Forwarded-For \$remote_addr;
      proxy_pass http://${TEMPS_CONTROL_INTERNAL_IP}:9001;
    }
  }
}
EOF

exec nginx -c /tmp/nginx.conf -g 'daemon off;'
