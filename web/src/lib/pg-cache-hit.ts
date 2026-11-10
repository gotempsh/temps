// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The Postgres cache hit ratio, as the metrics scraper computes it: block
 * hits divided by block accesses over a trailing window, never the lifetime
 * `pg_stat_database` counters. The window must match `CACHE_HIT_WINDOW` in
 * crates/temps-metrics/src/cache_hit_window.rs.
 */
export const PG_CACHE_HIT_RATIO_METRIC = 'pg.cache_hit_ratio'

/** Block accesses in the same window; the scraper writes it every scrape. */
export const PG_CACHE_BLOCKS_ACCESSED_METRIC = 'pg.cache_blocks_accessed'

export const PG_CACHE_HIT_WINDOW_MINUTES = 15

/**
 * The value a tile should show for `name`, read through `lookup`.
 *
 * The scraper writes no cache hit ratio for a window without block access,
 * so the latest stored ratio can be older than the window it is labelled
 * with. The block-access count is written on every scrape: when it says the
 * current window is empty, the ratio is "no data", whatever was stored
 * before.
 */
export function metricTileValue(
  name: string,
  lookup: (metric: string) => number | undefined
): number | undefined {
  const value = lookup(name)
  if (name !== PG_CACHE_HIT_RATIO_METRIC) return value
  return lookup(PG_CACHE_BLOCKS_ACCESSED_METRIC) === 0 ? undefined : value
}

/** Which databases a value covers: the whole server, or one database. */
export type PgCacheHitScope = { database: string } | 'all'

/** Short label for a chart or a tile whose scope is shown elsewhere. */
export function pgCacheHitRatioLabel(): string {
  return `Cache hit ratio (last ${PG_CACHE_HIT_WINDOW_MINUTES} min)`
}

/** Label for a tile, naming the databases the value covers. */
export function pgCacheHitRatioScopedLabel(scope: PgCacheHitScope): string {
  const where = scope === 'all' ? 'all databases' : scope.database
  return `Cache hit ratio (last ${PG_CACHE_HIT_WINDOW_MINUTES} min, ${where})`
}

/** What the number means, for a tooltip. */
export function pgCacheHitRatioDescription(scope: PgCacheHitScope): string {
  const where =
    scope === 'all'
      ? 'across all databases on this server, weighted by traffic'
      : `in database ${scope.database}`
  return (
    `Share of block reads served from shared buffers over the last ` +
    `${PG_CACHE_HIT_WINDOW_MINUTES} minutes, ${where}. Empty when nothing ` +
    `was read in that window.`
  )
}

/**
 * A 0–1 ratio as a percentage, or an em dash when there is no value.
 *
 * Not clamped: measured ratios are 0–1 by construction, and an alert-rule
 * threshold rendered here must show what was configured, so a rule entered
 * as `95` (meaning 95%) reads "9500.0%" instead of passing for "100.0%".
 */
export function formatPgCacheHitRatio(
  value: number | null | undefined
): string {
  if (value == null || !Number.isFinite(value)) return '—'
  return `${(value * 100).toFixed(1)}%`
}
