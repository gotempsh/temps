// Cross-signal "what changed" strip for a drilled-in metric.
//
// The correlation moat: Temps owns metrics, deploys, traces, and errors, so when
// a metric looks wrong the operator shouldn't have to re-pivot every other tool
// to the same window by hand. This strip leads with the deploy answer ("did a
// deploy land here?") — which the chart already marks — and offers one-click
// jumps to Traces and Errors pre-scoped to the SAME time window, so the next
// triage step is a click, not a re-filter.
//
// Honesty rules baked in:
//  - The deep-links carry `range` (and `env` for traces) that those pages
//    actually honour, so "Traces in this window" really is this window.
//  - We never invent a count we didn't fetch: the strip states what changed
//    (deploys, which we know) and links out for the rest rather than bluffing a
//    correlation we haven't computed.

import type { ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { useAlertStatus } from './alert-status'
import { Link } from 'react-router-dom'
import { ArrowUpRight, Bug, Eye, Network, Rocket } from 'lucide-react'

/** The relative ranges the explorer offers; a subset is shared with Traces. */
type TimeRange = '1h' | '6h' | '24h' | '7d' | '30d'

interface MetricCorrelationsProps {
  project: ProjectResponse
  metricName: string
  /** Aggregation token, so we only flag "firing" for a rule on THIS series. */
  aggregation: string
  /** The window the chart is showing — handed straight to the linked pages. */
  timeRange: TimeRange
  /** Selected environment id, or null for "all environments". */
  environmentId: number | null
  /** Deploy events that fall inside the visible window (already computed). */
  deployCount: number
}

/**
 * A horizontal "related signals" strip rendered under a metric's detail chart.
 * Self-resolves whether a rule on this series is firing (to frame the strip as
 * an investigation vs. a passive cross-link) — reuses the cached `listAlerts`,
 * no extra fetch.
 */
export function MetricCorrelations({
  project,
  metricName,
  aggregation,
  timeRange,
  environmentId,
  deployCount,
}: MetricCorrelationsProps) {
  const status = useAlertStatus(project.id).statusFor(metricName, aggregation)
  const firing = status === 'alert' || status === 'warn'

  const base = `/projects/${project.slug}`
  const env = environmentId ?? 'all'
  // Traces honour both; the errors page honours `range` (and widens 6h→24h
  // itself), so pass the metric's range through untouched.
  const tracesHref = `${base}/traces?range=${timeRange}&env=${env}`
  const errorsHref = `${base}/errors?range=${timeRange}`
  const observeHref = `${base}/observe`

  return (
    <div className="rounded-lg border border-border bg-card p-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 flex-col gap-1">
          <span className="text-xs font-medium text-foreground">
            {firing
              ? 'This metric is firing — see what else changed'
              : 'What changed in this window'}
          </span>
          <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Rocket className="size-3.5 shrink-0" />
            {deployCount > 0 ? (
              <span>
                <span className="font-medium text-foreground">
                  {deployCount} deploy{deployCount === 1 ? '' : 's'}
                </span>{' '}
                landed here — marked on the chart above.
              </span>
            ) : (
              <span>No deploys in this window — rules out a release.</span>
            )}
          </span>
        </div>

        <div className="flex shrink-0 flex-wrap items-center gap-2">
          <CorrelationLink to={tracesHref} icon={<Network className="size-3.5" />}>
            Traces
          </CorrelationLink>
          <CorrelationLink to={errorsHref} icon={<Bug className="size-3.5" />}>
            Errors
          </CorrelationLink>
          <CorrelationLink to={observeHref} icon={<Eye className="size-3.5" />}>
            Live view
          </CorrelationLink>
        </div>
      </div>
    </div>
  )
}

function CorrelationLink({
  to,
  icon,
  children,
}: {
  to: string
  icon: React.ReactNode
  children: React.ReactNode
}) {
  return (
    <Button
      asChild
      variant="outline"
      size="sm"
      className="h-7 gap-1.5 px-2.5 text-xs"
    >
      <Link to={to}>
        {icon}
        {children}
        <ArrowUpRight className="size-3 text-muted-foreground" />
      </Link>
    </Button>
  )
}
