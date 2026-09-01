// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getProjectsOptions,
  revenueGlobalEventsOptions,
  revenueMetricsGlobalMrrOptions,
  revenueMetricsGlobalSummaryOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { DateRangePicker } from '@/components/ui/date-range-picker'
import { EmptyState } from '@/components/ui/empty-state'
import {
  SearchableSelect,
  type SearchableSelectOption,
} from '@/components/ui/searchable-select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { DollarSign, X } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { DateRange } from 'react-day-picker'
import { Link } from 'react-router'

const ALL_PROJECTS = '__all_projects__'
const ALL_TYPES = '__all_types__'
const EVENT_TYPE_GROUPS: Array<{
  value: string
  label: string
  keywords?: string
}> = [
  { value: 'invoice.paid', label: 'Invoice paid', keywords: 'payment charge' },
  { value: 'charge.succeeded', label: 'Charge succeeded', keywords: 'payment' },
  { value: 'subscription.created', label: 'Subscription created' },
  { value: 'subscription.updated', label: 'Subscription updated' },
  { value: 'subscription.canceled', label: 'Subscription canceled' },
  { value: 'mrr.realized', label: 'MRR realized' },
  { value: 'refund.processed', label: 'Refund processed' },
]
const PAID_TYPES = ['invoice.paid', 'charge.succeeded']

export function Revenue() {
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle('Revenue')

  useEffect(() => {
    setBreadcrumbs([{ label: 'Revenue' }])
  }, [setBreadcrumbs])

  const [dateRange, setDateRange] = useState<DateRange | undefined>()
  const [projectFilter, setProjectFilter] = useState<string>(ALL_PROJECTS)
  const [typeFilter, setTypeFilter] = useState<string>(ALL_TYPES)

  const projectsQuery = useQuery({
    ...getProjectsOptions(),
  })

  const globalMrrQuery = useQuery({
    ...revenueMetricsGlobalMrrOptions(),
  })

  const globalSummaryQuery = useQuery({
    ...revenueMetricsGlobalSummaryOptions(),
  })

  const eventsQuery = useQuery(
    revenueGlobalEventsOptions({
      query: {
        project_id:
          projectFilter !== ALL_PROJECTS ? Number(projectFilter) : undefined,
        from: dateRange?.from ? dateRange.from.toISOString() : undefined,
        to: dateRange?.to ? dateRange.to.toISOString() : undefined,
        event_types:
          typeFilter === ALL_TYPES
            ? undefined
            : typeFilter === 'paid'
              ? PAID_TYPES.join(',')
              : typeFilter,
        limit: 200,
      },
    })
  )

  const projectOptions = useMemo<SearchableSelectOption[]>(() => {
    const opts: SearchableSelectOption[] = [
      { value: ALL_PROJECTS, label: 'All projects' },
    ]
    for (const p of projectsQuery.data?.projects ?? []) {
      opts.push({ value: String(p.id), label: p.name })
    }
    return opts
  }, [projectsQuery.data])

  const projectSlugById = useMemo(() => {
    const map = new Map<number, string>()
    for (const p of projectsQuery.data?.projects ?? []) {
      map.set(p.id, p.slug)
    }
    return map
  }, [projectsQuery.data])

  const typeOptions = useMemo<SearchableSelectOption[]>(() => {
    const opts: SearchableSelectOption[] = [
      { value: ALL_TYPES, label: 'All events' },
      { value: 'paid', label: 'Paid only (invoices + charges)' },
    ]
    for (const t of EVENT_TYPE_GROUPS) {
      opts.push({ value: t.value, label: t.label, keywords: t.keywords })
    }
    return opts
  }, [])

  const events = eventsQuery.data ?? []
  const hasFilters =
    !!dateRange || projectFilter !== ALL_PROJECTS || typeFilter !== ALL_TYPES

  const paidTotalMinor = useMemo(() => {
    let total = 0
    for (const e of events) {
      if (PAID_TYPES.includes(e.event_type) && e.amount_minor != null) {
        total += e.amount_minor
      }
    }
    return total
  }, [events])

  const paidCurrency = useMemo(() => {
    for (const e of events) {
      if (PAID_TYPES.includes(e.event_type) && e.currency) return e.currency
    }
    return (
      globalSummaryQuery.data?.currency ??
      globalMrrQuery.data?.currency ??
      'usd'
    )
  }, [events, globalMrrQuery.data, globalSummaryQuery.data])

  const resetFilters = () => {
    setDateRange(undefined)
    setProjectFilter(ALL_PROJECTS)
    setTypeFilter(ALL_TYPES)
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-1">
        <h1 className="text-2xl font-semibold tracking-tight">Revenue</h1>
        <p className="text-sm text-muted-foreground">
          Transactions, invoices, and subscription events across every project.
        </p>
      </div>

      <div className="grid gap-3 grid-cols-2 md:grid-cols-3 lg:grid-cols-6">
        <StatBlock
          label="Current MRR"
          value={
            globalSummaryQuery.isLoading
              ? null
              : formatMinor(
                  globalSummaryQuery.data?.current_mrr_minor ??
                    globalMrrQuery.data?.current_mrr_minor ??
                    0,
                  globalSummaryQuery.data?.currency ??
                    globalMrrQuery.data?.currency ??
                    'usd'
                )
          }
          hint={
            globalSummaryQuery.data
              ? `${globalSummaryQuery.data.active_subscriptions} active subs`
              : undefined
          }
        />
        <StatBlock
          label="Paid last 30d"
          value={
            globalSummaryQuery.isLoading
              ? null
              : formatMinor(
                  globalSummaryQuery.data?.paid_last_30d_minor ?? 0,
                  globalSummaryQuery.data?.currency ?? 'usd'
                )
          }
          hint={
            globalSummaryQuery.data
              ? `${globalSummaryQuery.data.transactions_last_30d} transactions`
              : undefined
          }
        />
        <StatBlock
          label="Refunded last 30d"
          value={
            globalSummaryQuery.isLoading
              ? null
              : formatMinor(
                  globalSummaryQuery.data?.refunded_last_30d_minor ?? 0,
                  globalSummaryQuery.data?.currency ?? 'usd'
                )
          }
          tone={
            (globalSummaryQuery.data?.refunded_last_30d_minor ?? 0) > 0
              ? 'warn'
              : undefined
          }
        />
        <StatBlock
          label="Paid all-time"
          value={
            globalSummaryQuery.isLoading
              ? null
              : formatMinor(
                  globalSummaryQuery.data?.paid_all_time_minor ?? 0,
                  globalSummaryQuery.data?.currency ?? 'usd'
                )
          }
          hint={
            globalSummaryQuery.data &&
            globalSummaryQuery.data.refunded_all_time_minor > 0
              ? `${formatMinor(
                  globalSummaryQuery.data.refunded_all_time_minor,
                  globalSummaryQuery.data.currency
                )} refunded`
              : undefined
          }
        />
        <StatBlock
          label="Active customers"
          value={
            globalSummaryQuery.isLoading
              ? null
              : String(globalSummaryQuery.data?.active_customers ?? 0)
          }
        />
        <StatBlock
          label="Paid in view"
          value={
            eventsQuery.isLoading
              ? null
              : formatMinor(paidTotalMinor, paidCurrency)
          }
          hint={`${events.filter((e) => PAID_TYPES.includes(e.event_type)).length} transactions${events.length >= 200 ? ' · first 200' : ''}`}
        />
      </div>

      <Card>
        <CardContent className="p-3">
          <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
            <DateRangePicker
              date={dateRange}
              onDateChange={setDateRange}
              className="w-full sm:w-[260px]"
            />
            <SearchableSelect
              value={projectFilter}
              onValueChange={setProjectFilter}
              options={projectOptions}
              placeholder="Filter by project"
              searchPlaceholder="Search projects..."
              emptyText="No projects."
              className="w-full sm:w-[220px]"
              disabled={projectsQuery.isLoading}
            />
            <SearchableSelect
              value={typeFilter}
              onValueChange={setTypeFilter}
              options={typeOptions}
              placeholder="Filter by event type"
              searchPlaceholder="Search types..."
              emptyText="No matching types."
              className="w-full sm:w-[240px]"
            />
            {hasFilters && (
              <Button
                variant="ghost"
                size="sm"
                onClick={resetFilters}
                className="ml-auto"
              >
                <X className="h-4 w-4 mr-1" />
                Clear
              </Button>
            )}
          </div>
        </CardContent>
      </Card>

      <Card>
        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Event</TableHead>
                <TableHead className="hidden md:table-cell">Project</TableHead>
                <TableHead className="hidden lg:table-cell">Customer</TableHead>
                <TableHead className="text-right">Amount</TableHead>
                <TableHead className="text-right">MRR Δ</TableHead>
                <TableHead className="text-right">When</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {eventsQuery.isLoading ? (
                Array.from({ length: 8 }).map((_, i) => (
                  <TableRow key={i}>
                    <TableCell>
                      <Skeleton className="h-5 w-32" />
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      <Skeleton className="h-4 w-24" />
                    </TableCell>
                    <TableCell className="hidden lg:table-cell">
                      <Skeleton className="h-4 w-28" />
                    </TableCell>
                    <TableCell>
                      <Skeleton className="ml-auto h-4 w-16" />
                    </TableCell>
                    <TableCell>
                      <Skeleton className="ml-auto h-4 w-12" />
                    </TableCell>
                    <TableCell>
                      <Skeleton className="ml-auto h-4 w-24" />
                    </TableCell>
                  </TableRow>
                ))
              ) : events.length === 0 ? (
                <TableRow>
                  <TableCell colSpan={6} className="p-0">
                    <EmptyState
                      icon={DollarSign}
                      title={
                        hasFilters
                          ? 'No revenue events match these filters'
                          : 'No revenue events yet'
                      }
                      description={
                        hasFilters
                          ? 'Try widening the date range or clearing filters.'
                          : 'Connect Stripe or LemonSqueezy on a project to start capturing invoices and subscription changes.'
                      }
                      action={
                        hasFilters ? (
                          <Button variant="outline" onClick={resetFilters}>
                            Clear filters
                          </Button>
                        ) : undefined
                      }
                    />
                  </TableCell>
                </TableRow>
              ) : (
                events.map((e) => (
                  <TableRow key={e.id}>
                    <TableCell>
                      <EventBadge type={e.event_type} />
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      {projectSlugById.has(e.project_id) ? (
                        <Link
                          to={`/projects/${projectSlugById.get(e.project_id)}`}
                          className="text-sm text-foreground hover:underline"
                        >
                          {e.project_name}
                        </Link>
                      ) : (
                        <span className="text-sm text-foreground">
                          {e.project_name}
                        </span>
                      )}
                    </TableCell>
                    <TableCell className="hidden lg:table-cell text-sm text-muted-foreground truncate max-w-[200px]">
                      {e.customer_ref ?? '—'}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {e.amount_minor != null && e.currency
                        ? formatMinor(e.amount_minor, e.currency)
                        : '—'}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      <MrrDelta minor={e.mrr_minor} currency={e.currency} />
                    </TableCell>
                    <TableCell className="text-right text-sm text-muted-foreground tabular-nums">
                      {format(new Date(e.occurred_at), 'MMM d, HH:mm')}
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </div>
      </Card>
    </div>
  )
}

function StatBlock({
  label,
  value,
  hint,
  tone,
}: {
  label: string
  value: string | null
  hint?: string
  tone?: 'warn'
}) {
  const valueClass =
    tone === 'warn'
      ? 'text-2xl font-semibold tracking-tight tabular-nums text-amber-600 dark:text-amber-400'
      : 'text-2xl font-semibold tracking-tight tabular-nums'
  return (
    <Card>
      <CardContent className="p-4 space-y-1">
        <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          {label}
        </p>
        {value == null ? (
          <Skeleton className="h-7 w-24" />
        ) : (
          <p className={valueClass}>{value}</p>
        )}
        {hint && (
          <p className="text-xs text-muted-foreground">{hint}</p>
        )}
      </CardContent>
    </Card>
  )
}

function EventBadge({ type }: { type: string }) {
  const tone: 'emerald' | 'blue' | 'red' | 'muted' = PAID_TYPES.includes(type)
    ? 'emerald'
    : type.startsWith('subscription.')
      ? 'blue'
      : type.startsWith('refund.') || type.endsWith('.canceled')
        ? 'red'
        : 'muted'
  const className =
    tone === 'emerald'
      ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400 border-emerald-500/20'
      : tone === 'blue'
        ? 'bg-blue-500/10 text-blue-600 dark:text-blue-400 border-blue-500/20'
        : tone === 'red'
          ? 'bg-red-500/10 text-red-600 dark:text-red-400 border-red-500/20'
          : 'bg-muted text-muted-foreground border-border'
  return (
    <Badge variant="outline" className={`font-mono text-[11px] ${className}`}>
      {type}
    </Badge>
  )
}

function MrrDelta({
  minor,
  currency,
}: {
  minor: number | null | undefined
  currency: string | null | undefined
}) {
  if (minor == null || minor === 0) return <span className="text-muted-foreground">—</span>
  const positive = minor > 0
  const cls = positive
    ? 'text-emerald-600 dark:text-emerald-400'
    : 'text-red-600 dark:text-red-400'
  return (
    <span className={cls}>
      {positive ? '+' : ''}
      {formatMinor(minor, currency ?? 'usd')}
    </span>
  )
}

function formatMinor(amountMinor: number, currency: string): string {
  try {
    return new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency: currency.toUpperCase(),
      maximumFractionDigits: 2,
    }).format(amountMinor / 100)
  } catch {
    return `${(amountMinor / 100).toFixed(2)} ${currency.toUpperCase()}`
  }
}

export default Revenue
