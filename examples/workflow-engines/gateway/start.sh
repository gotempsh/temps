#!/bin/sh
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
set -eu
case "${ENGINE_USER:-}" in ''|*[!a-zA-Z0-9_-]*) echo 'ENGINE_USER must contain letters, digits, underscores or hyphens' >&2; exit 1;; esac
if [ "${#ENGINE_PASSWORD}" -lt 24 ]; then echo 'ENGINE_PASSWORD must contain at least 24 characters' >&2; exit 1; fi
umask 077
hash=$(printf '%s\n' "$ENGINE_PASSWORD" | openssl passwd -6 -stdin)
printf '%s:%s\n' "$ENGINE_USER" "$hash" > /tmp/engine.htpasswd
unset ENGINE_PASSWORD hash
exec nginx -g 'daemon off;'
