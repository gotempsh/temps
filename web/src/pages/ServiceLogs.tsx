// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getServiceOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { DateRangePicker } from '@/components/ui/date-range-picker'
import { ServiceHealthBadge } from '@/components/storage/ServiceHealthCard'
import { ServiceLogo } from '@/components/ui/service-logo'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  type LogLevel,
  type LogSearchLine,
  useLogHistoryInfinite,
} from '@/hooks/useLogHistory'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowLeft,
  Database,
  Loader2,
  RefreshCw,
  ScrollText,
  Server,
} from 'lucide-react'
import type { DateRange } from 'react-day-picker'
import {
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import { Link, useParams } from 'react-router'

/** Tailwind classes per normalized level — mirrors the deployment log viewer. */
const LEVEL_CLASS: Record<LogLevel, string> = {
  ERROR: 'text-red-500',
  WARN: 'text-amber-500',
  INFO: 'text-foreground',
  DEBUG: 'text-muted-foreground',
  TRACE: 'text-muted-foreground/70',
}

const LEVELS: LogLevel[] = ['ERROR', 'WARN', 'INFO', 'DEBUG', 'TRACE']

/** Lines fetched per page. The viewer tails the newest page on load and walks
    older pages on scroll-to-top, so we deliberately do NOT load everything. */
const PAGE_SIZE = 200

/** Distance (px) from the top that triggers loading the next-older page. */
const LOAD_OLDER_THRESHOLD = 48

// ── Time range ─────────────────────────────────────────────────────────────

type TimePreset = '15m' | '1h' | '24h' | '7d' | 'custom'

const PRESETS: { value: TimePreset; label: string }[] = [
  { value: '15m', label: 'Last 15 minutes' },
  { value: '1h', label: 'Last hour' },
  { value: '24h', label: 'Last 24 hours' },
  { value: '7d', label: 'Last 7 days' },
  { value: 'custom', label: 'Custom range…' },
]

const PRESET_MS: Record<Exclude<TimePreset, 'custom'>, number> = {
  '15m': 15 * 60 * 1000,
  '1h': 60 * 60 * 1000,
  '24h': 24 * 60 * 60 * 1000,
  '7d': 7 * 24 * 60 * 60 * 1000,
}

function presetLabel(preset: TimePreset): string {
  return PRESETS.find((p) => p.value === preset)?.label ?? preset
}

function formatTs(ts: string): string {
  const d = new Date(ts)
  if (Number.isNaN(d.getTime())) return ts
  return d.toISOString().replace('T', ' ').replace('Z', '')
}

/**
 * Persisted, searchable log history for an imported/managed external service
 * (Postgres, MariaDB, Redis, MongoDB, MinIO). Reuses the same log-aggregator
 * search pipeline as application/deployment logs, scoped by
 * `external_service_id` instead of a project. Wears the same header shell as
 * the service detail page so it reads as one app.
 *
 * The log panel tails to the newest lines on load (like a terminal) and lazily
 * pages *older* lines in as the operator scrolls to the top, preserving scroll
 * position across each prepend so the viewport never jumps.
 */
export function ServiceLogs() {
  const { id } = useParams<{ id: string }>()
  const serviceId = id ? parseInt(id, 10) : NaN

  const { data: service, isLoading: serviceLoading } = useQuery({
    ...getServiceOptions({ path: { id: serviceId } }),
    enabled: !Number.isNaN(serviceId),
  })

  const [text, setText] = useState('')
  const [activeLevels, setActiveLevels] = useState<LogLevel[]>([])
  // Bumped by Refresh to collapse back to a single newest page and re-tail.
  const [refreshKey, setRefreshKey] = useState(0)

  // ── Date/time range filter ───────────────────────────────────────────
  const [preset, setPreset] = useState<TimePreset>('24h')
  const [customRange, setCustomRange] = useState<DateRange | undefined>(
    undefined
  )

  // Derive startTime / endTime from the active preset or custom range.
  // Memoized so the values are stable across renders (page identity must not
  // shift while the user is reading; only a filter change resets it).
  const { startTime, endTime } = useMemo(() => {
    if (preset !== 'custom') {
      const ms = PRESET_MS[preset]
      return {
        startTime: new Date(Date.now() - ms).toISOString(),
        endTime: undefined,
      }
    }
    return {
      startTime: customRange?.from?.toISOString(),
      endTime: customRange?.to?.toISOString(),
    }
  }, [preset, customRange])

  // When switching away from custom, reset the custom range so the DateRangePicker
  // comes up clean if the user switches back.
  function handlePresetChange(next: TimePreset) {
    if (next !== 'custom') setCustomRange(undefined)
    setPreset(next)
    // A time-range change should re-tail to the newest lines.
    setRefreshKey((k) => k + 1)
  }

  const levels = activeLevels.length ? activeLevels : undefined
  const trimmedText = text.trim() || undefined

  const {
    data,
    isLoading,
    isFetching,
    isFetchingNextPage,
    hasNextPage,
    fetchNextPage,
    error,
  } = useLogHistoryInfinite(
    {
      // projectId is ignored server-side when externalServiceId is set.
      projectId: 0,
      externalServiceId: serviceId,
      startTime,
      endTime,
      levels,
      text: trimmedText,
      pageSize: PAGE_SIZE,
      refreshKey,
    },
    !Number.isNaN(serviceId) && startTime !== undefined
  )

  // Pages come newest-first (page 0 = newest). Reverse so the oldest loaded
  // page sits at the top and the newest line lands at the bottom of the rope.
  const lines: LogSearchLine[] = useMemo(() => {
    const pages = data?.pages ?? []
    return [...pages].reverse().flatMap((p) => p.lines)
  }, [data])

  const svc = service?.service

  // ── Scroll management: tail-on-load + preserve-position-on-prepend ──────
  const scrollRef = useRef<HTMLDivElement | null>(null)
  // How many pages were rendered last commit — lets us distinguish the first
  // page (0→1, tail to bottom) from an older-page prepend (N→N+1, restore).
  const renderedPagesRef = useRef(0)
  // scrollHeight + scrollTop captured just before a fetchNextPage() so we can
  // re-anchor the viewport exactly after older lines prepend at the top.
  const pendingRestore = useRef<{ height: number; top: number } | null>(null)

  const pageCount = data?.pages.length ?? 0

  // A fresh query (filters changed or Refresh pressed) must re-tail from the
  // bottom. Reset the page counter whenever the query identity changes; this
  // effect is declared first so it runs before the scroll effect below.
  const querySig = `${serviceId}|${trimmedText ?? ''}|${
    levels?.join(',') ?? ''
  }|${refreshKey}`
  useLayoutEffect(() => {
    renderedPagesRef.current = 0
    pendingRestore.current = null
  }, [querySig])

  useLayoutEffect(() => {
    const el = scrollRef.current
    if (!el) return
    const prev = renderedPagesRef.current
    if (pageCount > prev) {
      if (prev === 0) {
        // First page of a fresh query → tail to the newest line.
        el.scrollTop = el.scrollHeight
      } else if (pendingRestore.current) {
        // Older page prepended → re-anchor the viewport on the line the user
        // was reading. New content added above shifts everything down by the
        // scrollHeight delta; offset scrollTop by exactly that to hold still.
        // (overflow-anchor:none on the container disables the browser's own
        // anchoring so this manual math is the sole authority.)
        const { height, top } = pendingRestore.current
        el.scrollTop = top + (el.scrollHeight - height)
        pendingRestore.current = null
      }
      renderedPagesRef.current = pageCount
    }
  }, [pageCount])

  const handleScroll = useCallback(() => {
    const el = scrollRef.current
    if (!el) return
    if (
      el.scrollTop <= LOAD_OLDER_THRESHOLD &&
      hasNextPage &&
      !isFetchingNextPage
    ) {
      // Snapshot height+top so the prepend effect can restore the anchor.
      pendingRestore.current = { height: el.scrollHeight, top: el.scrollTop }
      fetchNextPage()
    }
  }, [hasNextPage, isFetchingNextPage, fetchNextPage])

  function toggleLevel(level: LogLevel) {
    setActiveLevels((prev) =>
      prev.includes(level) ? prev.filter((l) => l !== level) : [...prev, level]
    )
  }

  // Refresh spins only for a full reload, not for background older-page loads.
  const refreshing = isFetching && !isFetchingNextPage

  // Human-readable summary of the active time window.
  const rangeLabel =
    preset !== 'custom'
      ? presetLabel(preset)
      : customRange?.from && customRange?.to
        ? `${customRange.from.toLocaleDateString()} – ${customRange.to.toLocaleDateString()}`
        : customRange?.from
          ? `From ${customRange.from.toLocaleDateString()}`
          : 'Custom range'

  return (
    <div className="flex-1 overflow-auto">
      <div className="p-4 space-y-6 md:p-6">
        {/* Header — mirrors ServiceDetail so the Logs view reads as one app. */}
        <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-3">
            <Link to={`/storage/${id}`}>
              <Button variant="ghost" size="icon">
                <ArrowLeft className="h-4 w-4" />
              </Button>
            </Link>
            {svc ? (
              <ServiceLogo service={svc.service_type} className="h-8 w-8" />
            ) : (
              <ScrollText className="h-8 w-8 text-muted-foreground" />
            )}
            <div className="flex flex-col gap-2">
              <div className="flex items-center gap-2 flex-wrap">
                <h1 className="text-xl font-semibold sm:text-2xl">
                  {svc?.name ?? id}
                </h1>
                {svc ? (
                  <>
                    <Badge
                      variant={
                        svc.status === 'running'
                          ? 'default'
                          : svc.status === 'stopped'
                            ? 'secondary'
                            : 'outline'
                      }
                      className="capitalize"
                    >
                      {svc.status}
                    </Badge>
                    {svc.status === 'running' ? (
                      <ServiceHealthBadge serviceId={serviceId} />
                    ) : null}
                    <Badge variant="outline" className="gap-1.5">
                      <ServiceLogo
                        service={svc.service_type}
                        className="h-3 w-3"
                      />
                      {svc.service_type}
                    </Badge>
                  </>
                ) : null}
                <Badge variant="outline" className="gap-1.5">
                  <ScrollText className="h-3 w-3" />
                  Logs
                </Badge>
              </div>
              {svc ? (
                <p className="text-sm text-muted-foreground">
                  Created <TimeAgo date={svc.created_at} />
                </p>
              ) : null}
            </div>
          </div>

          {/* Same action row as the detail page; Monitoring/Browse Data link
              back out, and Refresh replaces the kebab for this view. */}
          <div className="flex items-center gap-2 self-start sm:self-auto flex-wrap">
            {svc?.status === 'running' ? (
              <Link to={`/storage/${id}/monitoring`}>
                <Button variant="outline" size="sm" className="gap-2">
                  <Server className="h-4 w-4" />
                  <span className="hidden sm:inline">Monitoring</span>
                </Button>
              </Link>
            ) : null}
            <Link to={`/storage/${id}/browse`}>
              <Button variant="outline" size="sm" className="gap-2">
                <Database className="h-4 w-4" />
                <span className="hidden sm:inline">Browse Data</span>
              </Button>
            </Link>
            <Button
              variant="outline"
              size="sm"
              className="gap-2"
              onClick={() => setRefreshKey((k) => k + 1)}
              disabled={refreshing}
            >
              <RefreshCw
                className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`}
              />
              <span className="hidden sm:inline">Refresh</span>
            </Button>
          </div>
        </div>

        {/* Filter bar */}
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:flex-wrap">
          <Input
            placeholder="Filter logs (text search)…"
            value={text}
            onChange={(e) => setText(e.target.value)}
            className="w-full sm:w-[320px]"
          />

          {/* Time range preset selector */}
          <Select
            value={preset}
            onValueChange={(v) => handlePresetChange(v as TimePreset)}
          >
            <SelectTrigger className="w-full sm:w-[180px]">
              <SelectValue placeholder="Time range" />
            </SelectTrigger>
            <SelectContent>
              {PRESETS.map((p) => (
                <SelectItem key={p.value} value={p.value}>
                  {p.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>

          {/* Custom date/time picker — only shown when "Custom range…" is selected */}
          {preset === 'custom' ? (
            <DateRangePicker
              date={customRange}
              onDateChange={(range) => {
                setCustomRange(range)
                // Re-tail when a complete custom range is applied.
                if (range?.from && range?.to) {
                  setRefreshKey((k) => k + 1)
                }
              }}
              showTime
              className="w-full sm:w-auto"
            />
          ) : null}

          <div className="flex flex-wrap gap-1">
            {LEVELS.map((level) => (
              <Badge
                key={level}
                variant={activeLevels.includes(level) ? 'default' : 'outline'}
                className="cursor-pointer select-none"
                onClick={() => toggleLevel(level)}
              >
                {level}
              </Badge>
            ))}
          </div>
          <span className="text-xs text-muted-foreground sm:ml-auto">
            {rangeLabel} · {lines.length} line{lines.length === 1 ? '' : 's'}
            {hasNextPage ? '+' : ''}
          </span>
        </div>

        {/* Log panel — bounded height + its own scrollbar. Tails to the newest
            line on load; scrolling to the top lazily pages in older lines. */}
        {preset === 'custom' && !customRange?.from ? (
          <div className="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground">
            Select a start date above to load logs for a custom time range.
          </div>
        ) : error ? (
          <div className="rounded-md border border-destructive/40 bg-destructive/5 p-4 text-sm text-destructive">
            Failed to load logs: {(error as Error).message}
          </div>
        ) : isLoading || serviceLoading ? (
          <div className="space-y-2">
            {Array.from({ length: 12 }).map((_, i) => (
              <Skeleton key={i} className="h-5 w-full" />
            ))}
          </div>
        ) : lines.length === 0 ? (
          <div className="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground">
            No logs found for {rangeLabel.toLowerCase()}. Logs appear here once
            the service container emits output.
          </div>
        ) : (
          <div className="rounded-md border bg-muted/30">
            <div
              ref={scrollRef}
              onScroll={handleScroll}
              className="max-h-[calc(100vh-19rem)] overflow-auto [overflow-anchor:none]"
            >
              {/* Top affordance for the "load older" walk. */}
              {isFetchingNextPage ? (
                <div className="flex items-center justify-center gap-2 py-2 text-xs text-muted-foreground">
                  <Loader2 className="h-3 w-3 animate-spin" />
                  Loading older logs…
                </div>
              ) : hasNextPage ? (
                <div className="py-1.5 text-center text-xs text-muted-foreground/70">
                  Scroll up to load older logs
                </div>
              ) : (
                <div className="py-1.5 text-center text-xs text-muted-foreground/50">
                  Beginning of {rangeLabel.toLowerCase()}
                </div>
              )}
              <pre className="min-w-full px-3 pb-3 font-mono text-xs leading-relaxed">
                {lines.map((line, i) => (
                  <div
                    key={`${line.chunk_id}-${line.line_offset}-${i}`}
                    className="flex gap-3 border-b border-border/40 py-0.5 last:border-0"
                  >
                    <span className="shrink-0 text-muted-foreground/70">
                      {formatTs(line.timestamp)}
                    </span>
                    <span
                      className={`shrink-0 w-12 ${LEVEL_CLASS[line.level]}`}
                    >
                      {line.level}
                    </span>
                    <span
                      className={`whitespace-pre-wrap break-all ${LEVEL_CLASS[line.level]}`}
                    >
                      {line.message}
                    </span>
                  </div>
                ))}
              </pre>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
