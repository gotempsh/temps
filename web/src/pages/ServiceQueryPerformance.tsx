/**
 * ServiceQueryPerformance — dedicated Query Performance page for Postgres
 * external services.
 *
 * Route: /storage/:id/query-performance
 *
 * Layout: left mini-sidebar with section links, main content area.
 * Currently one section: Slow Queries (pg_stat_statements).
 * Built to accept additional sections (e.g. Index Usage, Vacuum Stats)
 * without restructuring.
 */

import {
  getServiceOptions,
  getSlowQueriesOptions,
  getSlowQueriesQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { SlowQueryRow } from '@/api/client/types.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
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
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowDown,
  ArrowLeft,
  ArrowUp,
  ChevronLeft,
  ChevronRight,
  Loader2,
  RefreshCw,
  Zap,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'

// ---------------------------------------------------------------------------
// Section registry — add new sections here without restructuring the page
// ---------------------------------------------------------------------------

type SectionId = 'slow-queries'

interface Section {
  id: SectionId
  label: string
  description: string
}

const SECTIONS: Section[] = [
  {
    id: 'slow-queries',
    label: 'Slow Queries',
    description: 'Top queries by mean execution time from pg_stat_statements',
  },
]

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function metricsErrorText(err: unknown): string {
  if (err == null) return ''
  if (typeof err === 'string') return err
  if (err instanceof Error) return err.message
  if (typeof err === 'object' && 'detail' in err) {
    return String((err as Record<string, unknown>).detail)
  }
  return JSON.stringify(err)
}

// ---------------------------------------------------------------------------
// ExtensionNotAvailable — error state with optional self-service restart
// ---------------------------------------------------------------------------

function isExtensionNotAvailableError(err: unknown): boolean {
  const msg = metricsErrorText(err).toLowerCase()
  return msg.includes('not available') || msg.includes('shared_preload')
}

function ExtensionNotAvailable({
  error,
  serviceTopology,
  onEnable,
  isEnabling,
}: {
  error: unknown
  serviceTopology: string
  onEnable: () => void
  isEnabling: boolean
}) {
  const isExtensionError = isExtensionNotAvailableError(error)
  const isStandalone = serviceTopology === 'standalone'

  return (
    <div className="flex flex-col items-center gap-4 rounded-lg border border-dashed py-10 text-center">
      <Zap className="size-6 text-muted-foreground" />

      {isExtensionError ? (
        <>
          <div className="max-w-sm space-y-1">
            <p className="text-sm font-medium">pg_stat_statements not loaded</p>
            <p className="text-xs text-muted-foreground">
              The extension requires{' '}
              <code className="font-mono">shared_preload_libraries=pg_stat_statements</code>{' '}
              to be set before Postgres starts.
            </p>
          </div>

          {isStandalone ? (
            <div className="flex flex-col items-center gap-2">
              <Button
                size="sm"
                onClick={onEnable}
                disabled={isEnabling}
              >
                {isEnabling ? (
                  <>
                    <Loader2 className="mr-2 size-3.5 animate-spin" />
                    Restarting…
                  </>
                ) : (
                  'Enable & Restart'
                )}
              </Button>
              <p className="text-xs text-muted-foreground">
                Or restart the container manually after adding the flag.
              </p>
            </div>
          ) : (
            <div className="max-w-sm space-y-1 text-left rounded-md border bg-muted/40 p-3 text-xs text-muted-foreground">
              <p className="font-medium text-foreground">Manual rolling restart required</p>
              <p>
                This is a clustered (HA) service. To avoid data loss, restart
                each node one at a time using your cluster management tooling.
                Ensure{' '}
                <code className="font-mono">shared_preload_libraries=pg_stat_statements</code>{' '}
                is set on every node before restarting.
              </p>
            </div>
          )}
        </>
      ) : (
        <p className="max-w-sm text-sm text-muted-foreground">
          {metricsErrorText(error)}
        </p>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// SlowQueriesContent
// ---------------------------------------------------------------------------

type SlowQuerySortKey =
  | 'calls'
  | 'total_exec_time_ms'
  | 'mean_exec_time_ms'
  | 'rows'
  | 'cache_hit_ratio'
type SortOrder = 'asc' | 'desc'

const PAGE_SIZE_OPTIONS = [5, 10, 20, 50, 100]

function SlowQueriesContent({
  serviceId,
  serviceName,
  serviceTopology,
}: {
  serviceId: number
  serviceName: string
  serviceTopology: string
}) {
  const queryClient = useQueryClient()
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(20)
  // Default matches the backend default (mean_exec_time_ms desc) so the
  // header arrow reflects the order actually applied on first load.
  const [sortKey, setSortKey] = useState<SlowQuerySortKey | null>(
    'mean_exec_time_ms',
  )
  const [sortOrder, setSortOrder] = useState<SortOrder>('desc')
  const [selectedRow, setSelectedRow] = useState<SlowQueryRow | null>(null)
  const [confirmOpen, setConfirmOpen] = useState(false)

  // Reset to page 1 when page size changes
  const handlePageSizeChange = (val: string) => {
    setPageSize(Number(val))
    setPage(1)
  }

  // Sorting is applied server-side (see the sort_by/sort_order query params
  // below) so the ordering stays consistent across pages — sorting only the
  // rows already fetched for the current page would show a different "top"
  // query depending on where the page boundary happened to fall.
  const handleSort = (key: SlowQuerySortKey) => {
    setPage(1)
    if (sortKey !== key) {
      setSortKey(key)
      setSortOrder('asc')
    } else if (sortOrder === 'asc') {
      setSortOrder('desc')
    } else {
      setSortKey(null)
    }
  }

  const queryParams = {
    page,
    page_size: pageSize,
    ...(sortKey != null ? { sort_by: sortKey, sort_order: sortOrder } : {}),
  }

  const { data, isLoading, error, isFetching } = useQuery({
    ...getSlowQueriesOptions({
      path: { service_id: serviceId },
      query: queryParams,
    }),
    staleTime: 30_000,
    retry: (failureCount, err) => {
      const msg = metricsErrorText(err).toLowerCase()
      if (msg.includes('not available') || msg.includes('shared_preload')) {
        return false
      }
      return failureCount < 2
    },
  })

  const handleRefresh = () => {
    queryClient.invalidateQueries({
      queryKey: getSlowQueriesQueryKey({
        path: { service_id: serviceId },
        query: queryParams,
      }),
    })
  }

  const enableMutation = useMutation({
    mutationFn: async () => {
      const response = await fetch(
        `/api/external-services/${serviceId}/pg-stat-statements/enable`,
        { method: 'POST', credentials: 'include' },
      )
      if (!response.ok) {
        const body = await response.json().catch(() => ({}))
        throw new Error(
          body.detail ?? 'Failed to enable pg_stat_statements',
        )
      }
      return response.json() as Promise<{ message: string }>
    },
    onSuccess: (data) => {
      toast.success('Container restarted', {
        description:
          data.message ??
          'pg_stat_statements is now active. Query data will appear shortly.',
      })
      // Give the container a moment, then refetch.
      setTimeout(() => handleRefresh(), 3000)
    },
    onError: (err: Error) => {
      toast.error('Enable failed', { description: err.message })
    },
  })

  const queries = data?.queries ?? []

  const totalCount = data?.total_count ?? 0
  const totalPages = pageSize > 0 ? Math.max(1, Math.ceil(totalCount / pageSize)) : 1
  const startRow = totalCount === 0 ? 0 : (page - 1) * pageSize + 1
  const endRow = Math.min(page * pageSize, totalCount)

  const SORT_COLUMNS: Array<{
    key: SlowQuerySortKey
    label: string
    className?: string
  }> = [
    { key: 'calls', label: 'Calls' },
    { key: 'total_exec_time_ms', label: 'Total (ms)' },
    { key: 'mean_exec_time_ms', label: 'Mean (ms)' },
    {
      key: 'rows',
      label: 'Rows',
      className: 'hidden md:table-cell',
    },
    {
      key: 'cache_hit_ratio',
      label: 'Cache Hit',
      className: 'hidden md:table-cell',
    },
  ]

  return (
    <div className="space-y-4">
      {/* Header row */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-base font-semibold">Slow Queries</h2>
          <p className="text-sm text-muted-foreground">
            Aggregate stats per normalized query, ordered by mean execution
            time. Click a row to see the full query text and all stats.
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          className="gap-1.5 shrink-0"
          onClick={handleRefresh}
          disabled={isFetching}
        >
          <RefreshCw
            className={cn('size-3.5', isFetching && 'animate-spin')}
          />
          <span className="hidden sm:inline">Refresh</span>
        </Button>
      </div>

      {/* Content */}
      {isLoading ? (
        <div className="space-y-2 rounded-lg border p-4">
          {Array.from({ length: 5 }).map((_, i) => (
            <div key={i} className="flex gap-3">
              <Skeleton className="h-4 flex-1" />
              <Skeleton className="h-4 w-16" />
              <Skeleton className="h-4 w-20" />
              <Skeleton className="h-4 w-20" />
            </div>
          ))}
        </div>
      ) : error != null ? (
        <ExtensionNotAvailable
          error={error}
          serviceTopology={serviceTopology}
          onEnable={() => setConfirmOpen(true)}
          isEnabling={enableMutation.isPending}
        />
      ) : queries.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed py-10 text-center">
          <Zap className="size-6 text-muted-foreground" />
          <p className="max-w-xs text-sm text-muted-foreground">
            No slow queries recorded yet. Queries appear here once{' '}
            <code className="font-mono text-xs">pg_stat_statements</code> has
            collected data.
          </p>
        </div>
      ) : (
        <>
          <div className="overflow-x-auto rounded-lg border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="min-w-[220px]">Query</TableHead>
                  <TableHead className="hidden md:table-cell">
                    Database
                  </TableHead>
                  {SORT_COLUMNS.map(({ key, label, className }) => (
                    <TableHead
                      key={key}
                      className={cn('tabular-nums', className)}
                    >
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => handleSort(key)}
                        className="-ml-3 h-8"
                      >
                        {label}
                        {sortKey === key ? (
                          sortOrder === 'asc' ? (
                            <ArrowUp className="ml-1 size-3.5" />
                          ) : (
                            <ArrowDown className="ml-1 size-3.5" />
                          )
                        ) : null}
                      </Button>
                    </TableHead>
                  ))}
                </TableRow>
              </TableHeader>
              <TableBody>
                {queries.map((row, i) => (
                  <TableRow
                    key={i}
                    className="cursor-pointer even:bg-muted/30 hover:bg-accent/40"
                    onClick={() => setSelectedRow(row)}
                  >
                    <TableCell className="max-w-xs">
                      <span className="block truncate font-mono text-xs text-foreground">
                        {row.query}
                      </span>
                    </TableCell>
                    <TableCell className="hidden text-sm md:table-cell">
                      {row.database}
                    </TableCell>
                    <TableCell className="tabular-nums text-sm">
                      {row.calls.toLocaleString()}
                    </TableCell>
                    <TableCell className="tabular-nums text-sm">
                      {row.total_exec_time_ms.toFixed(2)}
                    </TableCell>
                    <TableCell className="tabular-nums text-sm">
                      {row.mean_exec_time_ms.toFixed(2)}
                    </TableCell>
                    <TableCell className="hidden tabular-nums text-sm md:table-cell">
                      {row.rows.toLocaleString()}
                    </TableCell>
                    <TableCell className="hidden tabular-nums text-sm md:table-cell">
                      {row.cache_hit_ratio != null
                        ? `${(row.cache_hit_ratio * 100).toFixed(1)}%`
                        : '—'}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>

          {/* Pagination controls */}
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <span className="hidden sm:inline">
                Showing {startRow}–{endRow} of {totalCount.toLocaleString()}{' '}
                queries
              </span>
              <span className="sm:hidden">
                {page} / {totalPages}
              </span>
              <span className="hidden sm:inline text-muted-foreground/50">
                ·
              </span>
              <div className="flex items-center gap-1.5">
                <span className="text-xs">Per page:</span>
                <Select
                  value={String(pageSize)}
                  onValueChange={handlePageSizeChange}
                >
                  <SelectTrigger className="h-7 w-16 text-xs">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {PAGE_SIZE_OPTIONS.map((n) => (
                      <SelectItem key={n} value={String(n)}>
                        {n}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
            <div className="flex items-center gap-1">
              <Button
                variant="outline"
                size="icon"
                className="size-8"
                disabled={page <= 1}
                onClick={() => setPage((p) => Math.max(1, p - 1))}
              >
                <ChevronLeft className="size-4" />
              </Button>
              <span className="min-w-[4rem] text-center text-sm tabular-nums">
                {page} / {totalPages}
              </span>
              <Button
                variant="outline"
                size="icon"
                className="size-8"
                disabled={page >= totalPages}
                onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
              >
                <ChevronRight className="size-4" />
              </Button>
            </div>
          </div>
        </>
      )}

      {/* Detail sheet */}
      <QueryDetailSheet
        row={selectedRow}
        open={selectedRow !== null}
        onClose={() => setSelectedRow(null)}
      />

      {/* Enable & Restart confirmation dialog */}
      <AlertDialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Enable pg_stat_statements?</AlertDialogTitle>
            <AlertDialogDescription>
              This will restart the{' '}
              <strong>{serviceName}</strong> database container to load{' '}
              <code>pg_stat_statements</code>. Active connections will be
              briefly dropped and in-flight transactions rolled back. The
              database will come back online in a few seconds.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                setConfirmOpen(false)
                enableMutation.mutate()
              }}
            >
              {enableMutation.isPending ? (
                <>
                  <Loader2 className="mr-2 size-4 animate-spin" />
                  Restarting…
                </>
              ) : (
                'Enable & Restart'
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

// ---------------------------------------------------------------------------
// QueryDetailSheet — full query text + all stats
// ---------------------------------------------------------------------------

function QueryDetailSheet({
  row,
  open,
  onClose,
}: {
  row: SlowQueryRow | null
  open: boolean
  onClose: () => void
}) {
  return (
    <Sheet open={open} onOpenChange={(o) => !o && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-2xl">
        <SheetHeader className="mb-4">
          <SheetTitle>Query Detail</SheetTitle>
          <SheetDescription>
            Aggregate statistics from pg_stat_statements for this normalized
            query fingerprint.
          </SheetDescription>
        </SheetHeader>

        {row && (
          <div className="space-y-6">
            {/* Full query text */}
            <div>
              <p className="mb-2 text-xs font-semibold uppercase tracking-widest text-muted-foreground">
                Query
              </p>
              <pre className="overflow-x-auto rounded-lg border bg-muted/50 p-4 font-mono text-xs leading-relaxed whitespace-pre-wrap break-all">
                {row.query}
              </pre>
            </div>

            {/* Stats grid */}
            <div>
              <p className="mb-3 text-xs font-semibold uppercase tracking-widest text-muted-foreground">
                Statistics
              </p>
              <dl className="grid grid-cols-2 gap-3 sm:grid-cols-3">
                <StatCell
                  label="Calls"
                  value={row.calls.toLocaleString()}
                />
                <StatCell
                  label="Total Time"
                  value={`${row.total_exec_time_ms.toFixed(3)} ms`}
                />
                <StatCell
                  label="Mean Time"
                  value={`${row.mean_exec_time_ms.toFixed(3)} ms`}
                />
                <StatCell
                  label="Rows"
                  value={row.rows.toLocaleString()}
                />
                <StatCell
                  label="Rows / Call"
                  value={
                    row.calls > 0
                      ? (row.rows / row.calls).toFixed(1)
                      : '—'
                  }
                />
                <StatCell
                  label="Cache Hit Ratio"
                  value={
                    row.cache_hit_ratio != null
                      ? `${(row.cache_hit_ratio * 100).toFixed(2)}%`
                      : '—'
                  }
                  badge={
                    row.cache_hit_ratio != null
                      ? row.cache_hit_ratio >= 0.95
                        ? 'good'
                        : row.cache_hit_ratio >= 0.8
                          ? 'warn'
                          : 'bad'
                      : undefined
                  }
                />
              </dl>
            </div>
          </div>
        )}
      </SheetContent>
    </Sheet>
  )
}

function StatCell({
  label,
  value,
  badge,
}: {
  label: string
  value: string
  badge?: 'good' | 'warn' | 'bad'
}) {
  return (
    <div className="rounded-lg border bg-card p-3">
      <dt className="text-[10px] font-medium uppercase tracking-widest text-muted-foreground">
        {label}
      </dt>
      <dd className="mt-1 flex items-center gap-2">
        <span className="tabular-nums text-sm font-semibold">{value}</span>
        {badge === 'good' && (
          <Badge
            variant="outline"
            className="text-[10px] text-green-600 border-green-200 bg-green-50"
          >
            good
          </Badge>
        )}
        {badge === 'warn' && (
          <Badge
            variant="outline"
            className="text-[10px] text-amber-600 border-amber-200 bg-amber-50"
          >
            low
          </Badge>
        )}
        {badge === 'bad' && (
          <Badge
            variant="outline"
            className="text-[10px] text-red-600 border-red-200 bg-red-50"
          >
            poor
          </Badge>
        )}
      </dd>
    </div>
  )
}

// ---------------------------------------------------------------------------
// ServiceQueryPerformance — page root
// ---------------------------------------------------------------------------

export function ServiceQueryPerformance() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  const serviceId = id ? parseInt(id, 10) : 0

  const [activeSection, setActiveSection] = useState<SectionId>('slow-queries')

  const { data: serviceData, isLoading: serviceLoading } = useQuery({
    ...getServiceOptions({ path: { id: serviceId } }),
    enabled: !!serviceId,
  })

  const serviceName = serviceData?.service?.name ?? 'Service'

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Databases', href: '/storage' },
      { label: serviceName, href: `/storage/${serviceId}` },
      { label: 'Query Performance' },
    ])
  }, [setBreadcrumbs, serviceName, serviceId])

  usePageTitle(
    serviceData?.service?.name
      ? `${serviceData.service.name} · Query Performance`
      : 'Query Performance',
  )

  return (
    <div className="flex min-h-full flex-col">
      {/* Back nav */}
      <div className="border-b bg-background px-4 py-3 sm:px-6">
        <div className="flex items-center gap-3">
          <Button
            variant="ghost"
            size="sm"
            className="gap-1.5 text-muted-foreground"
            onClick={() => navigate(`/storage/${serviceId}`)}
          >
            <ArrowLeft className="size-4" />
            <span className="hidden sm:inline">{serviceName}</span>
          </Button>
          <span className="text-muted-foreground/40">/</span>
          <span className="text-sm font-medium">Query Performance</span>
        </div>
      </div>

      {/* Body: sidebar + content */}
      <div className="flex flex-1 flex-col lg:flex-row">
        {/* Mini sidebar */}
        <nav
          aria-label="Query performance sections"
          className="shrink-0 border-b lg:w-52 lg:border-b-0 lg:border-r"
        >
          <ul className="flex flex-row gap-1 overflow-x-auto p-2 lg:flex-col lg:overflow-visible">
            {SECTIONS.map((section) => (
              <li key={section.id}>
                <button
                  type="button"
                  onClick={() => setActiveSection(section.id)}
                  className={cn(
                    'w-full rounded-md px-3 py-2 text-left text-sm transition-colors',
                    activeSection === section.id
                      ? 'bg-accent font-medium text-accent-foreground'
                      : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground',
                  )}
                >
                  {section.label}
                </button>
              </li>
            ))}
          </ul>
        </nav>

        {/* Main content */}
        <main className="flex-1 overflow-auto p-4 sm:p-6">
          {serviceLoading ? (
            <div className="space-y-4">
              <Skeleton className="h-6 w-40" />
              <Skeleton className="h-4 w-72" />
              <Skeleton className="h-48 w-full" />
            </div>
          ) : activeSection === 'slow-queries' ? (
            <SlowQueriesContent
              serviceId={serviceId}
              serviceName={serviceName}
              serviceTopology={serviceData?.service?.topology ?? 'standalone'}
            />
          ) : null}
        </main>
      </div>
    </div>
  )
}
