# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Bun telemetry-api — anonymous product-telemetry ingest for Temps.
# Multi-stage: install deps with a cached layer, then a slim runtime.
FROM oven/bun:1.3-alpine AS deps
WORKDIR /app
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile --production

FROM oven/bun:1.3-alpine AS runtime
WORKDIR /app
ENV NODE_ENV=production
# Listen on the port Temps assigns (PORT is injected); default 4200 locally.
ENV PORT=4200

COPY --from=deps /app/node_modules ./node_modules
COPY package.json bun.lock tsconfig.json ./
COPY src ./src
COPY migrations ./migrations
# GeoLite2 DB for anonymous country lookup (gitignored; provisioned locally /
# at build). geo.ts reads GEOLITE2_COUNTRY_DB (default /app/data/...).
# The build FAILS if the DB is missing: at runtime geo.ts only logs a warning
# and stores every event with a null country, which previously shipped to
# production unnoticed for weeks when an image was built from a checkout
# without data/. Place GeoLite2-Country.mmdb in data/ before building.
COPY data ./data
RUN test -s /app/data/GeoLite2-Country.mmdb || { \
      echo "ERROR: data/GeoLite2-Country.mmdb is missing or empty; country geolocation would be disabled. Copy the GeoLite2 Country DB into telemetry-api/data/ and rebuild." >&2; \
      exit 1; \
    }
COPY docker-entrypoint.sh ./docker-entrypoint.sh
RUN chmod +x ./docker-entrypoint.sh

EXPOSE 4200

# Migrate-then-serve. Migrations are idempotent (telemetry_migrations table),
# so a redeploy is safe.
ENTRYPOINT ["./docker-entrypoint.sh"]
