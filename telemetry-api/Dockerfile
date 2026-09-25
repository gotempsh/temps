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
# GeoLite2 DB for anonymous country lookup (gitignored; place
# GeoLite2-Country.mmdb in data/ before building). geo.ts reads
# GEOLITE2_COUNTRY_DB (default /app/data/GeoLite2-Country.mmdb). The server
# REFUSES TO START in production if that DB is missing, corrupt, or can't
# resolve countries, so a bad image fails its deploy health check instead of
# silently storing NULL countries. Set GEOLITE2_COUNTRY_DB=disabled to run
# without geolocation on purpose.
COPY data ./data
COPY docker-entrypoint.sh ./docker-entrypoint.sh
RUN chmod +x ./docker-entrypoint.sh

EXPOSE 4200

# Migrate-then-serve. Migrations are idempotent (telemetry_migrations table),
# so a redeploy is safe.
ENTRYPOINT ["./docker-entrypoint.sh"]
