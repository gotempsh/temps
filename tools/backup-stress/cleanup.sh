#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
docker compose --project-directory "$SCRIPT_DIR" -f "$SCRIPT_DIR/docker-compose.yml" down --volumes --remove-orphans

for name in temps-backup-bench-restore-100m temps-backup-bench-restore-200m; do
  docker rm --force "$name" >/dev/null 2>&1 || true
done
for name in temps-backup-bench-restore-100m temps-backup-bench-restore-200m; do
  docker volume rm "$name" >/dev/null 2>&1 || true
done
