#!/bin/sh
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -e

# Run migrations (idempotent) before starting the server. If migrations fail,
# the container exits non-zero so the platform surfaces the failure rather than
# serving against an un-migrated schema.
echo "[entrypoint] running migrations..."
bun run src/db/migrate.ts

echo "[entrypoint] starting telemetry-api on port ${PORT:-4200}..."
exec bun run src/index.ts
