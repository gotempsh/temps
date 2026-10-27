// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Engines the metrics scraper has a collector for (see
 *  `crates/temps-metrics/src/scraper.rs`). Each has its own metric layout in
 *  the monitoring views. */
export type CollectedMetricsEngine =
  'postgres' | 'redis' | 'mongodb' | 'mariadb' | 's3' | 'rustfs'

/** `unsupported`: no engine metrics are collected for this service type; only
 *  the container CPU/memory the health monitor samples for every service. */
export type ServiceMetricsEngine = CollectedMetricsEngine | 'unsupported'

const COLLECTED_ENGINES: readonly CollectedMetricsEngine[] = [
  'postgres',
  'redis',
  'mongodb',
  'mariadb',
  's3',
  'rustfs',
]

function isCollectedEngine(value: string): value is CollectedMetricsEngine {
  return (COLLECTED_ENGINES as readonly string[]).includes(value)
}

/** Which metric layout a service's monitoring views should use.
 *
 *  A service type without a collector maps to `unsupported` rather than to
 *  another engine's layout: rendering it as Postgres showed a panel of empty
 *  Postgres tiles with no hint that nothing would ever fill them. */
export function serviceMetricsEngine(
  serviceType: string,
  dockerImage?: string | null
): ServiceMetricsEngine {
  const type = serviceType.trim().toLowerCase()
  // An s3 service running the RustFS image reports RustFS metrics.
  if (type === 's3' && dockerImage?.toLowerCase().includes('rustfs'))
    return 'rustfs'
  return isCollectedEngine(type) ? type : 'unsupported'
}
