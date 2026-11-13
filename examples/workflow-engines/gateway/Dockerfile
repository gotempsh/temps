# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
FROM nginx:1.28.0-alpine@sha256:30f1c0d78e0ad60901648be663a710bdadf19e4c10ac6782c235200619158284
RUN apk add --no-cache openssl && rm /etc/nginx/conf.d/default.conf
COPY nginx.conf /etc/nginx/nginx.conf
ARG ENGINE=restate
RUN case "$ENGINE" in restate) sed -i 's|ENGINE_ADMIN_UPSTREAM|http://restate:9070|;s|ENGINE_INGRESS_UPSTREAM|http://restate:8080|;s|ENGINE_HEALTH_UPSTREAM|http://restate:9070/health|' /etc/nginx/nginx.conf ;; temporal) sed -i 's|ENGINE_ADMIN_UPSTREAM|http://ui:8080|;s|ENGINE_INGRESS_UPSTREAM|http://ui:8080|;s|ENGINE_HEALTH_UPSTREAM|http://ui:8080/health|' /etc/nginx/nginx.conf ;; *) exit 1 ;; esac
COPY start.sh /start.sh
USER nginx
ENTRYPOINT ["/bin/sh", "/start.sh"]
