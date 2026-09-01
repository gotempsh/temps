// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Shared alert-status model for the metrics surfaces.
//
// One `listAlerts` fetch, turned into a Datadog-style status model that the
// health header, metric tiles, and severity sort all consume: a per-rule status
// (Alert / Warn / No data / OK), worst-first ranking, severity counts, and a
// per-metric lookup keyed on (metric_name, aggregation) so a tile only reds out
// for a rule that actually targets the series it shows.

import { listAlertsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { OtelMetricAlertRuleResponse } from '@/api/client'
import type { MetricTone } from '@/components/charts/metric-sparkline'
import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'

/** Datadog-style monitor status, worst → best. */
export type AlertStatusLevel = 'alert' | 'warn' | 'nodata' | 'ok'

const STATUS_ORDER: AlertStatusLevel[] = ['alert', 'warn', 'nodata', 'ok']

/** Per-status display metadata (label, chart tone, status-dot color, badge). */
export const STATUS_META: Record<
  AlertStatusLevel,
  {
    label: string
    tone: MetricTone
    /** Tailwind bg class for the status dot. */
    dotClass: string
    badgeVariant: 'destructive' | 'warning' | 'secondary' | 'success'
  }
> = {
  alert: {
    label: 'Alert',
    tone: 'poor',
    dotClass: 'bg-destructive',
    badgeVariant: 'destructive',
  },
  warn: {
    label: 'Warn',
    tone: 'warn',
    dotClass: 'bg-warning',
    badgeVariant: 'warning',
  },
  nodata: {
    label: 'No data',
    tone: 'neutral',
    dotClass: 'bg-muted-foreground',
    badgeVariant: 'secondary',
  },
  ok: { label: 'OK', tone: 'good', dotClass: 'bg-success', badgeVariant: 'success' },
}

/** The status of a single rule from its evaluator state + severity. */
export function ruleStatus(rule: OtelMetricAlertRuleResponse): AlertStatusLevel {
  // A disabled monitor cannot be firing. The backend evaluator only scans
  // enabled rules, so a rule disabled mid-firing keeps a frozen
  // `last_state: 'firing'` forever — trust `enabled` over that stale state, or
  // we'd flash a red alarm for a monitor the operator deliberately turned off.
  if (!rule.enabled) return 'ok'
  if (rule.last_state === 'firing') {
    return rule.severity === 'critical' ? 'alert' : 'warn'
  }
  // `unknown` = never evaluated / anomaly still gathering a baseline.
  if (rule.last_state === 'unknown') return 'nodata'
  return 'ok'
}

/**
 * Open-series count for a metric, from whichever backing rule is a dynamic
 * (per-series) rule with something currently firing — `null` when none of the
 * rules are dynamic-and-firing, so callers fall back to the flat status label.
 * Powers the "N series breaching" tooltip variant (ADR-026 Phase 4); there's
 * no "of M" total since the frontend only sees currently-firing series, not
 * how many series were evaluated this tick.
 */
export function dynamicFiringSeriesCount(
  rules: OtelMetricAlertRuleResponse[],
): number | null {
  const rule = rules.find(
    (r) => r.dynamic_alerts && (r.firing_series ?? []).length > 0,
  )
  return rule ? (rule.firing_series ?? []).length : null
}

/** The worse (lower-ordered) of two statuses. */
export function worstOf(
  a: AlertStatusLevel | undefined,
  b: AlertStatusLevel,
): AlertStatusLevel {
  if (a === undefined) return b
  return STATUS_ORDER.indexOf(a) <= STATUS_ORDER.indexOf(b) ? a : b
}

/** Sort key (lower = worse) for ranking rules/metrics worst-first. */
export function statusRank(level: AlertStatusLevel): number {
  return STATUS_ORDER.indexOf(level)
}

/** The roll-up of a set of metric tiles to one Datadog-style status. */
export interface StatusRollup {
  /** Worst status across the distinct rules, or null when none map to a rule. */
  level: AlertStatusLevel | null
  counts: Record<AlertStatusLevel, number>
  /** Distinct rules currently firing (alert + warn). */
  firing: number
  /** Distinct rules watching these tiles (the denominator for "all clear"). */
  watched: number
}

/**
 * Roll a set of `(metric, aggregation)` tile references up to a single status —
 * the worst monitor status across the DISTINCT rules backing those tiles, plus
 * counts. Mirrors how Datadog derives a dashboard's status from its widgets'
 * monitors. Counts are de-duplicated by rule id, so three tiles plotting one
 * firing metric read as "1 firing", not "3" (the name-fallback in `rulesFor`
 * would otherwise map all three to the same rule). Tiles with no rule contribute
 * nothing, so `level` stays null when NOTHING on the dashboard is watched —
 * letting the caller stay honest (render nothing) over a vanity "all clear".
 */
export function rollupStatus(
  tiles: Array<{ metricName: string; aggregation?: string }>,
  rulesFor: (
    metricName: string,
    aggregation?: string,
  ) => OtelMetricAlertRuleResponse[],
): StatusRollup {
  const counts: Record<AlertStatusLevel, number> = {
    alert: 0,
    warn: 0,
    nodata: 0,
    ok: 0,
  }
  let level: AlertStatusLevel | null = null
  const seen = new Set<number>()
  for (const t of tiles) {
    for (const rule of rulesFor(t.metricName, t.aggregation)) {
      if (seen.has(rule.id)) continue
      seen.add(rule.id)
      const s = ruleStatus(rule)
      counts[s]++
      level = worstOf(level ?? undefined, s)
    }
  }
  return { level, counts, firing: counts.alert + counts.warn, watched: seen.size }
}

export interface AlertStatus {
  isLoading: boolean
  isError: boolean
  rules: OtelMetricAlertRuleResponse[]
  /** All rules sorted worst-first (alert → warn → nodata → ok, then by name). */
  ranked: OtelMetricAlertRuleResponse[]
  /** Currently-firing rules (alert + warn), worst-first. */
  firing: OtelMetricAlertRuleResponse[]
  /** Anomaly rules still building a baseline (`unknown`). */
  gathering: OtelMetricAlertRuleResponse[]
  counts: Record<AlertStatusLevel, number>
  hasRules: boolean
  /**
   * Worst status of rules on a metric. Prefers an exact (metric, aggregation)
   * match; falls back to any rule on the metric name (so a tile can show "a rule
   * on this metric is firing" without claiming THIS series is the one breaching).
   */
  statusFor: (
    metricName: string,
    aggregation?: string,
  ) => AlertStatusLevel | null
  /**
   * The rule rows backing a metric — exact (metric, aggregation) match if any,
   * else every rule on the metric name (mirrors `statusFor`'s fallback). Used to
   * roll a dashboard up by DISTINCT rule rather than by tile.
   */
  rulesFor: (
    metricName: string,
    aggregation?: string,
  ) => OtelMetricAlertRuleResponse[]
}

/** Fetch the project's alert rules once and derive the status model. */
export function useAlertStatus(
  projectId: number,
  opts?: { enabled?: boolean },
): AlertStatus {
  const query = useQuery({
    ...listAlertsOptions({ query: { project_id: projectId } }),
    enabled: (opts?.enabled ?? true) && !!projectId,
  })

  return useMemo<AlertStatus>(() => {
    const rules = query.data?.data ?? []
    const ranked = [...rules].sort(
      (a, b) =>
        statusRank(ruleStatus(a)) - statusRank(ruleStatus(b)) ||
        a.metric_name.localeCompare(b.metric_name),
    )
    const counts: Record<AlertStatusLevel, number> = {
      alert: 0,
      warn: 0,
      nodata: 0,
      ok: 0,
    }
    for (const r of rules) counts[ruleStatus(r)]++

    const byKey = new Map<string, AlertStatusLevel>()
    const byKeyRules = new Map<string, OtelMetricAlertRuleResponse[]>()
    const byName = new Map<string, OtelMetricAlertRuleResponse[]>()
    for (const r of rules) {
      const status = ruleStatus(r)
      const key = `${r.metric_name} ${r.aggregation}`
      byKey.set(key, worstOf(byKey.get(key), status))
      const keyArr = byKeyRules.get(key) ?? []
      keyArr.push(r)
      byKeyRules.set(key, keyArr)
      const arr = byName.get(r.metric_name) ?? []
      arr.push(r)
      byName.set(r.metric_name, arr)
    }

    return {
      isLoading: query.isPending,
      isError: query.isError,
      rules,
      ranked,
      // A disabled rule's `last_state` is frozen, so gate on `enabled` too —
      // otherwise a monitor switched off mid-firing lingers in these lists.
      firing: ranked.filter((r) => r.enabled && r.last_state === 'firing'),
      gathering: ranked.filter(
        (r) =>
          r.enabled &&
          r.detection_config.kind === 'anomaly' &&
          r.last_state === 'unknown',
      ),
      counts,
      hasRules: rules.length > 0,
      statusFor: (metricName, aggregation) => {
        if (aggregation) {
          const exact = byKey.get(`${metricName} ${aggregation}`)
          if (exact) return exact
        }
        const all = byName.get(metricName)
        if (!all || all.length === 0) return null
        return all.map(ruleStatus).reduce<AlertStatusLevel>(worstOf, 'ok')
      },
      rulesFor: (metricName, aggregation) => {
        if (aggregation) {
          const exact = byKeyRules.get(`${metricName} ${aggregation}`)
          if (exact && exact.length > 0) return exact
        }
        return byName.get(metricName) ?? []
      },
    }
  }, [query.data, query.isPending, query.isError])
}
