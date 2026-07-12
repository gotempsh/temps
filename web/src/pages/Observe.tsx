import { ProjectResponse } from '@/api/client'
import { usePageTitle } from '@/hooks/usePageTitle'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  type ObserveFilters,
  type TimeRange,
  TIME_RANGES,
} from '@/components/observe/ObserveFilterBar'
import { ObservePanel } from '@/components/observe/panels/ObservePanel'
import { useObserveQuery } from '@/components/observe/useObserveQuery'
import {
  ALL_KINDS,
  type EventKind,
  type ObservabilityEvent,
} from '@/components/observe/types'
import {
  AlertOctagon,
  Bot,
  CircleDollarSign,
  Inbox,
  Network,
  Search,
  Workflow,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { cn } from '@/lib/utils'
import { format } from 'date-fns'

interface ObserveProps {
  project: ProjectResponse
}

/**
 * Kinds enabled on first visit. Traces are intentionally OFF by default —
 * `otel_spans` is a TimescaleDB hypertable that can hold millions of rows
 * per project, and pulling spans on every Observe page load would make
 * the merged query and the rendered list unnecessarily expensive. The
 * user can toggle Traces on via the cockpit card.
 *
 * Runtime logs are not part of Observe at all — they live on the
 * dedicated Logs page.
 */
const DEFAULT_KINDS: readonly EventKind[] = ['request', 'error', 'revenue']

/** Order-insensitive kind-set equality used to decide whether to omit
 *  the `kinds` URL param when filters match the defaults. */
function sameKindSet(
  a: readonly EventKind[],
  b: readonly EventKind[],
): boolean {
  if (a.length !== b.length) return false
  const set = new Set(a)
  return b.every((k) => set.has(k))
}

/**
 * Unified observability page — one list, one detail panel, one filter
 * surface. Each row carries everything the panel needs so opening detail
 * doesn't trigger a follow-up fetch in the common case.
 *
 * Layout is two-tier:
 *   1. Cockpit header — 5 sparklines (one per kind) over the selected
 *      time range, each clickable to toggle that kind. Surfaces "where
 *      should I look first" before the eye even reaches the stream.
 *   2. Console stream — terminal-style monospace rows with a colored
 *      gutter per kind. Designed for high-density scanning of mixed
 *      event types.
 *
 * Filter state lives in URL search params so the page is shareable:
 * "here's the link, look at the 500s after the deploy at 14:02".
 */
export default function Observe({ project }: ObserveProps) {
  usePageTitle(`Observe · ${project.name}`)
  const [searchParams, setSearchParams] = useSearchParams()
  const [selectedEvent, setSelectedEvent] =
    useState<ObservabilityEvent | null>(null)

  const filters: ObserveFilters = useMemo(() => {
    const kindsParam = searchParams.get('kinds')
    const kinds = kindsParam
      ? (kindsParam
          .split(',')
          .map((k) => k.trim())
          .filter((k): k is EventKind =>
            (ALL_KINDS as readonly string[]).includes(k),
          ) as EventKind[])
      : ([...DEFAULT_KINDS] as EventKind[])

    const timeRangeParam = searchParams.get('time_range') as TimeRange | null
    const timeRange: TimeRange =
      timeRangeParam && TIME_RANGES.some((r) => r.value === timeRangeParam)
        ? timeRangeParam
        : '24h'

    // Bots hidden by default (mirrors ResourceMonitoring). Only the
    // explicit `hide_bots=false` URL param flips it off so the default
    // URL stays clean.
    const hideBots = searchParams.get('hide_bots') !== 'false'

    return {
      kinds: kinds.length > 0 ? kinds : ([...DEFAULT_KINDS] as EventKind[]),
      timeRange,
      search: searchParams.get('search') ?? '',
      environmentId: searchParams.get('environment_id')
        ? Number(searchParams.get('environment_id'))
        : null,
      hideBots,
    }
  }, [searchParams])

  const updateFilters = (next: ObserveFilters) => {
    const params = new URLSearchParams(searchParams)
    if (sameKindSet(next.kinds, DEFAULT_KINDS)) {
      params.delete('kinds')
    } else {
      params.set('kinds', next.kinds.join(','))
    }
    if (next.timeRange === '24h') {
      params.delete('time_range')
    } else {
      params.set('time_range', next.timeRange)
    }
    if (next.search) {
      params.set('search', next.search)
    } else {
      params.delete('search')
    }
    if (next.environmentId != null) {
      params.set('environment_id', String(next.environmentId))
    } else {
      params.delete('environment_id')
    }
    // Bots hidden is the default — only persist when the user has flipped
    // it off so the URL doesn't carry redundant state.
    if (next.hideBots) {
      params.delete('hide_bots')
    } else {
      params.set('hide_bots', 'false')
    }
    setSearchParams(params, { replace: true })
  }

  const fromDate = useMemo(
    () => timeRangeToFromDate(filters.timeRange),
    [filters.timeRange],
  )

  const query = useObserveQuery({
    projectId: project.id,
    kinds: filters.kinds,
    from: fromDate,
    environmentId: filters.environmentId ?? undefined,
    search: filters.search || undefined,
    limit: 100,
    hideBots: filters.hideBots,
  })

  const events = query.data?.events ?? []

  const onRowClick = (event: ObservabilityEvent) => setSelectedEvent(event)

  return (
    <>
      <div className="flex h-full w-full flex-col bg-background">
        <CockpitHeader
          project={project}
          filters={filters}
          onChange={updateFilters}
          events={events}
        />
        {query.isPending ? (
          <ListSkeleton />
        ) : query.isError ? (
          <ErrorState message={query.error} />
        ) : events.length === 0 ? (
          <EmptyState />
        ) : (
          <div className="flex-1 overflow-y-auto font-mono text-xs">
            {events.map((event) => (
              <ConsoleRow
                key={`${event.type}-${eventId(event)}`}
                event={event}
                onClick={() => onRowClick(event)}
              />
            ))}
          </div>
        )}
      </div>

      <ObservePanel
        event={selectedEvent}
        open={selectedEvent !== null}
        onOpenChange={(open) => {
          if (!open) setSelectedEvent(null)
        }}
        projectSlug={project.slug}
      />
    </>
  )
}

// ─── Shared helpers ────────────────────────────────────────────────────────

function eventId(event: ObservabilityEvent): string | number {
  // Use the strongest stable identifier we have per kind so React's keys
  // don't collide when two rows share a timestamp.
  const e = event as unknown as Record<string, unknown>
  return (
    (e.id as string | number | undefined) ??
    (e.span_id as string | undefined) ??
    (e.error_group_id as string | undefined) ??
    `${event.type}-${event.ts}`
  )
}

function timeRangeToFromDate(range: TimeRange): Date {
  const now = new Date()
  const map: Record<TimeRange, number> = {
    '15m': 15 * 60 * 1000,
    '1h': 60 * 60 * 1000,
    '24h': 24 * 60 * 60 * 1000,
    '7d': 7 * 24 * 60 * 60 * 1000,
    '30d': 30 * 24 * 60 * 60 * 1000,
  }
  return new Date(now.getTime() - map[range])
}

const KIND_META: Record<
  EventKind,
  {
    label: string
    icon: React.ComponentType<{ className?: string }>
    accent: string // tailwind text color for the kind's accent
    gutter: string // tailwind border color for the console gutter
  }
> = {
  request: {
    label: 'Requests',
    icon: Network,
    accent: 'text-sky-500',
    gutter: 'border-l-sky-500',
  },
  span: {
    label: 'Traces',
    icon: Workflow,
    accent: 'text-violet-500',
    gutter: 'border-l-violet-500',
  },
  error: {
    label: 'Errors',
    icon: AlertOctagon,
    accent: 'text-rose-500',
    gutter: 'border-l-rose-500',
  },
  revenue: {
    label: 'Revenue',
    icon: CircleDollarSign,
    accent: 'text-emerald-500',
    gutter: 'border-l-emerald-500',
  },
}

// ─── Cockpit header ────────────────────────────────────────────────────────

/** Top strip with one mini sparkline per kind. Clicking a card toggles its
 *  kind in the filter so the user can read the chart and act in the same
 *  motion. */
function CockpitHeader({
  project,
  filters,
  onChange,
  events,
}: {
  project: ProjectResponse
  filters: ObserveFilters
  onChange: (next: ObserveFilters) => void
  events: ObservabilityEvent[]
}) {
  const buckets = useMemo(() => bucketByKind(events, 24), [events])

  const toggle = (kind: EventKind) => {
    const next = filters.kinds.includes(kind)
      ? filters.kinds.filter((k) => k !== kind)
      : [...filters.kinds, kind]
    onChange({
      ...filters,
      kinds: next.length === 0 ? [...DEFAULT_KINDS] : next,
    })
  }

  return (
    <div className="flex flex-col gap-3 border-b border-border bg-background p-4 text-foreground">
      <div className="flex flex-col gap-1">
        <p className="text-xs uppercase tracking-wide text-muted-foreground">
          Observe
        </p>
        <h1 className="text-lg font-semibold tracking-tight">
          {project.name}
        </h1>
      </div>

      <div className="grid grid-cols-2 gap-2 sm:grid-cols-2 lg:grid-cols-4">
        {ALL_KINDS.map((kind) => {
          const meta = KIND_META[kind]
          const series = buckets[kind] ?? []
          const total = series.reduce((sum, n) => sum + n, 0)
          const active = filters.kinds.includes(kind)
          const Icon = meta.icon
          return (
            <button
              key={kind}
              type="button"
              onClick={() => toggle(kind)}
              aria-pressed={active}
              title={
                active
                  ? `Hide ${meta.label}`
                  : `Load ${meta.label} (off by default)`
              }
              className={cn(
                'group flex flex-col gap-2 rounded-lg border p-3 text-left transition-colors',
                active
                  ? 'border-border bg-card'
                  : 'border-dashed border-border bg-background opacity-60 hover:opacity-100',
              )}
            >
              <div className="flex items-center justify-between gap-2">
                <div className="flex min-w-0 items-center gap-2">
                  <Icon className={cn('size-4 shrink-0', meta.accent)} />
                  <span className="truncate text-sm font-medium">
                    {meta.label}
                  </span>
                </div>
                <span className="font-mono text-sm tabular-nums text-muted-foreground">
                  {active ? total : 'off'}
                </span>
              </div>
              <Sparkline values={series} accent={meta.accent} />
            </button>
          )
        })}
      </div>

      <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
        <div className="relative flex-1">
          <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            type="search"
            placeholder="grep path / class / event…"
            value={filters.search}
            onChange={(e) =>
              onChange({ ...filters, search: e.target.value })
            }
            className="pl-9 font-mono"
          />
        </div>
        <Select
          value={filters.timeRange}
          onValueChange={(v) =>
            onChange({ ...filters, timeRange: v as TimeRange })
          }
        >
          <SelectTrigger className="w-full font-mono sm:w-[160px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {TIME_RANGES.map((r) => (
              <SelectItem key={r.value} value={r.value}>
                {r.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button
          type="button"
          variant={filters.hideBots ? 'default' : 'outline'}
          size="sm"
          aria-pressed={filters.hideBots}
          title={
            filters.hideBots
              ? 'Bot/crawler requests hidden — click to show'
              : 'Bot/crawler requests visible — click to hide'
          }
          onClick={() =>
            onChange({ ...filters, hideBots: !filters.hideBots })
          }
          className="gap-1.5"
        >
          <Bot className="size-3.5" />
          {filters.hideBots ? 'Hide bots' : 'Show bots'}
        </Button>
      </div>
    </div>
  )
}

function Sparkline({
  values,
  accent,
}: {
  values: number[]
  accent: string
}) {
  const max = Math.max(1, ...values)
  return (
    <div
      className="flex h-8 items-end gap-px"
      role="img"
      aria-label={`${values.length} buckets`}
    >
      {values.map((v, i) => (
        <div
          key={i}
          className={cn(
            'flex-1 rounded-sm bg-current opacity-40 transition-opacity group-hover:opacity-70',
            accent,
          )}
          style={{ height: `${Math.max(8, (v / max) * 100)}%` }}
        />
      ))}
    </div>
  )
}

/** Group events into N evenly spaced time buckets per kind. Cheap O(n). */
function bucketByKind(
  events: ObservabilityEvent[],
  bucketCount: number,
): Record<EventKind, number[]> {
  const out: Record<EventKind, number[]> = {
    request: new Array(bucketCount).fill(0),
    span: new Array(bucketCount).fill(0),
    error: new Array(bucketCount).fill(0),
    revenue: new Array(bucketCount).fill(0),
  }
  if (events.length === 0) return out

  const tsList = events.map((e) => new Date(e.ts).getTime())
  const min = Math.min(...tsList)
  const max = Math.max(...tsList)
  const span = Math.max(1, max - min)

  for (const e of events) {
    const ts = new Date(e.ts).getTime()
    const idx = Math.min(
      bucketCount - 1,
      Math.floor(((ts - min) / span) * bucketCount),
    )
    out[e.type][idx] += 1
  }
  return out
}

// ─── Console row ───────────────────────────────────────────────────────────

function ConsoleRow({
  event,
  onClick,
}: {
  event: ObservabilityEvent
  onClick: () => void
}) {
  const meta = KIND_META[event.type]
  const ts = new Date(event.ts)
  const { primary, suffix } = consoleSummary(event)
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'group flex w-full items-baseline gap-3 border-b border-border border-l-2 px-4 py-1.5 text-left text-foreground hover:bg-muted/60',
        meta.gutter,
      )}
    >
      <time
        dateTime={event.ts}
        title={format(ts, 'yyyy-MM-dd HH:mm:ss.SSS')}
        className="w-20 shrink-0 text-muted-foreground tabular-nums"
      >
        {format(ts, 'HH:mm:ss')}
      </time>
      <span className={cn('w-16 shrink-0 uppercase', meta.accent)}>
        {event.type}
      </span>
      <span className="min-w-0 flex-1 truncate">{primary}</span>
      {suffix && (
        <span className="shrink-0 text-muted-foreground tabular-nums">{suffix}</span>
      )}
    </button>
  )
}

/** One-line console summary per event type. Pure presentation. */
function consoleSummary(event: ObservabilityEvent): {
  primary: string
  suffix: string | null
} {
  switch (event.type) {
    case 'request':
      return {
        primary: `${event.method} ${event.path}`,
        suffix:
          event.latency_ms != null
            ? `${event.status} · ${event.latency_ms}ms`
            : `${event.status}`,
      }
    case 'span':
      return {
        primary: `${event.service} ${event.operation}`,
        suffix:
          event.duration_ms != null
            ? `${event.duration_ms.toFixed(1)}ms`
            : null,
      }
    case 'error':
      return {
        primary: `${event.error_class}: ${event.message ?? ''}`.trim(),
        suffix: null,
      }
    case 'revenue':
      return {
        primary: `${event.event_type}${
          event.customer_ref ? ` · ${event.customer_ref}` : ''
        }`,
        suffix:
          event.amount_minor != null
            ? formatMoney(event.amount_minor, event.currency)
            : null,
      }
  }
}

function formatMoney(minor: number, currency: string | null | undefined) {
  const major = minor / 100
  if (!currency) return major.toFixed(2)
  try {
    return new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency: currency.toUpperCase(),
    }).format(major)
  } catch {
    return `${major.toFixed(2)} ${currency.toUpperCase()}`
  }
}

// ─── Status states ─────────────────────────────────────────────────────────

function ListSkeleton() {
  return (
    <div className="flex-1 overflow-hidden">
      {Array.from({ length: 12 }).map((_, i) => (
        <div
          key={i}
          className="flex items-center gap-3 border-b border-border px-4 py-2"
        >
          <Skeleton className="h-3 w-16" />
          <Skeleton className="h-5 w-5 rounded" />
          <Skeleton className="h-4 flex-1" />
          <Skeleton className="h-5 w-12 rounded" />
        </div>
      ))}
    </div>
  )
}

function EmptyState() {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-2 p-12 text-center text-muted-foreground">
      <Inbox className="h-8 w-8" />
      <p className="text-sm">No events match the current filters.</p>
      <p className="text-xs">Try widening the time range or toggling kinds.</p>
    </div>
  )
}

function ErrorState({ message }: { message: string }) {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-2 p-12 text-center">
      <p className="text-sm font-medium text-rose-500">
        Failed to load events
      </p>
      <p className="text-xs text-muted-foreground">{message}</p>
    </div>
  )
}
