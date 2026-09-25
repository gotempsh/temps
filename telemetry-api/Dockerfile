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
# GEOLITE2_COUNTRY_DB (default /app/data/GeoLite2-Country.mmdb). Without it
# every event would silently store a NULL country, so it is enforced twice:
# - at build: verify-geo.ts opens the bundled DB and resolves a probe IP, so a
#   checkout without it fails here. Images that mount the DB at runtime (via
#   GEOLITE2_COUNTRY_DB) skip this with --build-arg GEOLITE2_VERIFY_AT_BUILD=0.
# - at startup: the server runs the same probe in production and refuses to
#   start, failing the deploy's health check, if the DB is unusable.
COPY data ./data
ARG GEOLITE2_VERIFY_AT_BUILD=1
RUN if [ "$GEOLITE2_VERIFY_AT_BUILD" = "1" ]; then bun src/verify-geo.ts; fi
COPY docker-entrypoint.sh ./docker-entrypoint.sh
RUN chmod +x ./docker-entrypoint.sh

EXPOSE 4200

# Migrate-then-serve. Migrations are idempotent (telemetry_migrations table),
# so a redeploy is safe.
ENTRYPOINT ["./docker-entrypoint.sh"]
