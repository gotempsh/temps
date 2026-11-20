#!/bin/sh
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
set -eu
: "${SQL_PASSWORD:?Database password is required}"
for database in temporal temporal_visibility; do
    temporal-sql-tool --plugin postgres12 --ep postgres -u temporal -p 5432 --db "$database" create
    temporal-sql-tool --plugin postgres12 --ep postgres -u temporal -p 5432 --db "$database" setup-schema -v 0.0
    directory=temporal
    if [ "$database" = temporal_visibility ]; then directory=visibility; fi
    temporal-sql-tool --plugin postgres12 --ep postgres -u temporal -p 5432 --db "$database" update-schema -d "/etc/temporal/schema/postgresql/v12/$directory/versioned"
done
