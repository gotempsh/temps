#!/bin/sh
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# The host explicitly marks Git routing ownership, independently of the CA.
# Check on every invocation so an already-open terminal sees newly issued access.
# A failed/expired attachment must fail at the proxy, never retry with credentials
# from the project's independent Git credential helper.
if [ -f /run/temps-service-access/github-git ]; then
    /usr/bin/git \
        -c http.https://github.com/.proxy=http://127.0.0.1:3128 \
        -c http.https://github.com/.sslCAInfo=/run/temps-service-access/ca.pem \
        -c http.https://github.com/.sslVerify=true \
        -c credential.https://github.com.helper= \
        "$@"
    result=$?
    if [ "$result" -ne 0 ] && [ ! -S /run/temps-service-access/proxy.sock ]; then
        # A missing attachment alone cannot distinguish expiry, revocation,
        # pending delivery or a worker outage. Preserve Git's original error.
        printf '%s\n' \
            'Vault access is unavailable: this sandbox has no active proxy attachment.' \
            'Check Vault sessions in the sandbox details. If the session expired or was revoked, approve a new session and retry.' \
            'If a session is still active, check its connection status and the worker; do not replace your GitHub credential.' >&2
    fi
    exit "$result"
fi
exec /usr/bin/git "$@"
