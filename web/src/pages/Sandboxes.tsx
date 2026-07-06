import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Box,
  ChevronDown,
  ExternalLink,
  Play,
  RefreshCw,
  RotateCw,
  Square,
  Timer,
  Trash2,
} from 'lucide-react'
import { toast } from 'sonner'

import { usePageTitle } from '@/hooks/usePageTitle'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
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
import {
  extendTimeoutMutation,
  listSandboxesOptions,
  pauseSandboxMutation,
  restartSandboxMutation,
  resumeSandboxMutation,
  stopSandboxMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { toSandboxView, type SandboxView } from '@/components/sandboxes/helpers'
import { CreateSandboxDocs } from '@/components/sandboxes/CreateSandboxDocs'

function statusVariant(
  status: string
): 'default' | 'secondary' | 'success' | 'warning' | 'destructive' | 'outline' {
  switch (status) {
    case 'running':
      return 'success'
    case 'stopped':
      return 'warning'
    case 'destroyed':
      return 'destructive'
    default:
      return 'outline'
  }
}

// Dev-server defaults the Open Preview dropdown surfaces as quick picks.
// Kept in sync with SandboxDetail — if a port family is added there, add
// it here so the list row and the detail page stay consistent.
const DEFAULT_PORTS: { port: number; label: string }[] = [
  { port: 3000, label: 'Next.js · Node' },
  { port: 5173, label: 'Vite' },
  { port: 8080, label: 'Generic HTTP' },
  { port: 8000, label: 'Django · FastAPI' },
  { port: 4000, label: 'Phoenix · Keystone' },
  { port: 4200, label: 'Angular' },
  { port: 3001, label: 'Alt Node' },
]

function formatCountdown(iso: string, now: number): string {
  const diffMs = new Date(iso).getTime() - now
  if (diffMs <= 0) return 'expired'
  const secs = Math.floor(diffMs / 1000)
  const d = Math.floor(secs / 86400)
  const h = Math.floor((secs % 86400) / 3600)
  const m = Math.floor((secs % 3600) / 60)
  const s = secs % 60
  if (d >= 1) return `${d}d ${h}h`
  if (h >= 1) return `${h}h ${m}m`
  if (m >= 1) return `${m}m ${s}s`
  return `${s}s`
}

function formatAge(iso: string, now: number): string {
  const diffMs = now - new Date(iso).getTime()
  if (diffMs < 0) {
    try {
      return new Date(iso).toLocaleString()
    } catch {
      return iso
    }
  }
  const secs = Math.floor(diffMs / 1000)
  if (secs < 60) return `${secs}s ago`
  const mins = Math.floor(secs / 60)
  if (mins < 60) return `${mins}m ago`
  const hours = Math.floor(mins / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.floor(hours / 24)
  return `${days}d ago`
}

/// Tick every second so the per-row countdown feels live. A single
/// top-level tick drives every row — cheaper than each row owning its
/// own interval, and all rows stay in visual sync.
function useNow(enabled: boolean) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!enabled) return
    const id = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(id)
  }, [enabled])
  return now
}

const PAGE_SIZE = 20

type StatusFilter = 'active' | 'expired' | 'all'

// Expired = either status explicitly says so OR the timer has elapsed.
// Stopped sandboxes whose timer is still running count as active — they
// can still be resumed. Destroyed rows are hidden from Active but live
// under Expired for audit.
function isExpired(s: SandboxView, now: number): boolean {
  if (s.status === 'destroyed') return true
  return new Date(s.expires_at).getTime() <= now
}

export default function Sandboxes() {
  usePageTitle('Sandboxes')
  const [page, setPage] = useState(1)
  const [filter, setFilter] = useState<StatusFilter>('active')
  const [stopTarget, setStopTarget] = useState<SandboxView | null>(null)
  const queryClient = useQueryClient()

  const listQuery = listSandboxesOptions({
    query: { page, page_size: PAGE_SIZE },
  })
  const { data, isLoading, isError, error, refetch, isFetching } = useQuery({
    ...listQuery,
    refetchInterval: 15_000,
  })

  const items: SandboxView[] = (data?.sandboxes ?? []).map(toSandboxView)
  const hasNext = data?.pagination?.next != null
  const hasPrev = data?.pagination?.prev != null

  // Only tick when at least one row has a live countdown to render. Avoids
  // pointless re-renders on an all-destroyed page.
  const needsTick = items.some((s) => s.status !== 'destroyed')
  const now = useNow(needsTick)

  // Bucketing is derived from `now`, so it naturally refreshes as the
  // countdown ticks — a row crossing its expiry moves to the Expired tab
  // on the next second. Counts are page-local (matches the paginated
  // items we actually have); acceptable until we add server-side filtering.
  const { visible, activeCount, expiredCount } = useMemo(() => {
    let active = 0
    let expired = 0
    const visible: SandboxView[] = []
    for (const s of items) {
      const exp = isExpired(s, now)
      if (exp) expired += 1
      else active += 1
      if (
        filter === 'all' ||
        (filter === 'active' && !exp) ||
        (filter === 'expired' && exp)
      ) {
        visible.push(s)
      }
    }
    return { visible, activeCount: active, expiredCount: expired }
  }, [items, now, filter])

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ['sandboxes'] })

  const deleteMutation = useMutation({
    ...stopSandboxMutation(),
    meta: { errorTitle: 'Failed to delete sandbox' },
    onSuccess: () => {
      invalidate()
      setStopTarget(null)
      toast.success('Sandbox deleted')
    },
  })

  return (
    <div className="mx-auto w-full max-w-7xl p-4 sm:p-6 lg:p-8 space-y-6">
      {/* Page header — canonical pattern from DESIGN.md §4.4 */}
      <div className="flex flex-col gap-4 sm:flex-row sm:items-end sm:justify-between">
        <div className="space-y-1">
          <h1 className="text-2xl font-semibold tracking-tight">Sandboxes</h1>
          <p className="text-sm text-muted-foreground">
            Standalone containers for one-off commands, tests, or agent work.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {/* Segmented filter — defaults to Active so expired/destroyed rows
              don't clutter the everyday view, but stay one click away for
              cleanup or audit. Counts are computed from the current page. */}
          {items.length > 0 && (
            <div className="inline-flex rounded-md border bg-background p-0.5">
              {(
                [
                  { key: 'active', label: 'Active', count: activeCount },
                  { key: 'expired', label: 'Expired', count: expiredCount },
                  { key: 'all', label: 'All', count: items.length },
                ] as const
              ).map((tab) => {
                const selected = filter === tab.key
                return (
                  <button
                    key={tab.key}
                    type="button"
                    onClick={() => setFilter(tab.key)}
                    className={`rounded px-2.5 py-1 text-xs font-medium transition-colors ${
                      selected
                        ? 'bg-muted text-foreground'
                        : 'text-muted-foreground hover:text-foreground'
                    }`}
                    aria-pressed={selected}
                  >
                    {tab.label}
                    <span className="ml-1 tabular-nums text-muted-foreground">
                      {tab.count}
                    </span>
                  </button>
                )
              })}
            </div>
          )}
          <Button
            variant="outline"
            size="sm"
            onClick={() => refetch()}
            disabled={isFetching}
          >
            <RefreshCw
              className={`mr-1.5 h-4 w-4 ${isFetching ? 'animate-spin' : ''}`}
            />
            <span className="hidden sm:inline">Refresh</span>
          </Button>
        </div>
      </div>

      {isLoading ? (
        <div className="space-y-3">
          {Array.from({ length: 4 }).map((_, i) => (
            <Card key={i}>
              <CardContent className="py-4 space-y-3">
                <div className="flex items-center justify-between gap-3">
                  <div className="flex-1 space-y-2">
                    <div className="h-5 w-48 rounded bg-muted animate-pulse" />
                    <div className="h-3 w-32 rounded bg-muted animate-pulse" />
                  </div>
                  <div className="flex gap-2">
                    <div className="h-8 w-20 rounded bg-muted animate-pulse" />
                    <div className="h-8 w-20 rounded bg-muted animate-pulse" />
                  </div>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      ) : isError ? (
        <Card>
          <CardContent className="py-12 text-center space-y-2">
            <Box className="mx-auto h-8 w-8 text-destructive" />
            <p className="text-sm font-medium">Failed to load sandboxes</p>
            <p className="text-xs text-muted-foreground">
              {(error as Error)?.message ?? 'Unknown error'}
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => refetch()}
              className="mt-2"
            >
              <RefreshCw className="mr-1.5 h-4 w-4" />
              Try again
            </Button>
          </CardContent>
        </Card>
      ) : items.length === 0 ? (
        // Empty state — there's no "Create Sandbox" button in the UI yet, so
        // surface the three real ways to create one (CLI / REST / SDK) right
        // here instead of making the user hunt for docs.
        <CreateSandboxDocs variant="full" />
      ) : (
        <div className="space-y-3">
          {/* Collapsible docs banner. Creation still only happens from outside
              the UI, so keep the instructions one click away even when the
              user already has sandboxes. */}
          <CreateSandboxDocs variant="compact" />
          {visible.length === 0 ? (
            <Card>
              <CardContent className="py-10 text-center space-y-2">
                <Box className="mx-auto h-6 w-6 text-muted-foreground" />
                <p className="text-sm font-medium">No {filter} sandboxes</p>
                <p className="text-xs text-muted-foreground">
                  {filter === 'active'
                    ? 'All sandboxes on this page have expired.'
                    : 'Nothing to show in this view.'}
                </p>
                {filter !== 'all' && (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setFilter('all')}
                    className="mt-2"
                  >
                    Show all
                  </Button>
                )}
              </CardContent>
            </Card>
          ) : (
            visible.map((sbx) => (
              <SandboxRow
                key={sbx.id}
                sandbox={sbx}
                now={now}
                onDeleteRequest={setStopTarget}
              />
            ))
          )}
        </div>
      )}

      {(hasNext || hasPrev) && (
        <div className="flex flex-col gap-2 border-t pt-4 sm:flex-row sm:items-center sm:justify-between">
          <p className="text-xs text-muted-foreground tabular-nums">
            Page {page}
          </p>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={!hasPrev}
              onClick={() => setPage((p) => Math.max(1, p - 1))}
            >
              Previous
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={!hasNext}
              onClick={() => setPage((p) => p + 1)}
            >
              Next
            </Button>
          </div>
        </div>
      )}

      <AlertDialog
        open={stopTarget !== null}
        onOpenChange={(open) => {
          if (!open) setStopTarget(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete sandbox?</AlertDialogTitle>
            <AlertDialogDescription>
              This tears down the container for{' '}
              <span className="font-mono">{stopTarget?.id}</span>. The row is
              kept for audit but cannot be restarted. This cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (stopTarget)
                  deleteMutation.mutate({ path: { id: stopTarget.id } })
              }}
              disabled={deleteMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deleteMutation.isPending ? 'Deleting…' : 'Delete'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

/**
 * One sandbox row-card. Each row owns its own mutations so a slow
 * operation on one sandbox doesn't block actions on another (a pending
 * `Stop` on row A keeps row B's buttons live). The per-row mutation
 * objects are lightweight — React Query dedupes internally.
 *
 * Kept structurally parallel to SandboxDetail's header + status strip:
 * identity on the left, action cluster on the right, live countdown +
 * extend chips below. Clicking anywhere outside an interactive control
 * navigates into the detail page.
 */
function SandboxRow({
  sandbox,
  now,
  onDeleteRequest,
}: {
  sandbox: SandboxView
  now: number
  onDeleteRequest: (s: SandboxView) => void
}) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [customPort, setCustomPort] = useState('')

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ['sandboxes'] })

  const pauseMutation = useMutation({
    ...pauseSandboxMutation(),
    meta: { errorTitle: 'Failed to stop sandbox' },
    onSuccess: () => {
      invalidate()
      toast.success('Sandbox stopped')
    },
  })

  const resumeMutation = useMutation({
    ...resumeSandboxMutation(),
    meta: { errorTitle: 'Failed to resume sandbox' },
    onSuccess: () => {
      invalidate()
      toast.success('Sandbox resumed')
    },
  })

  const restartMutation = useMutation({
    ...restartSandboxMutation(),
    meta: { errorTitle: 'Failed to restart sandbox' },
    onSuccess: () => {
      invalidate()
      toast.success('Sandbox restarted')
    },
  })

  const extendMutation = useMutation({
    ...extendTimeoutMutation(),
    meta: { errorTitle: 'Failed to extend timeout' },
    onSuccess: (_data, vars) => {
      invalidate()
      const secs = vars.body?.extra_secs ?? 0
      toast.success(
        `Timeout extended by ${secs >= 3600 ? `${secs / 3600}h` : `${secs / 60}m`}`
      )
    },
  })

  const running = sandbox.status === 'running'
  const stopped = sandbox.status === 'stopped'
  const destroyed = sandbox.status === 'destroyed'
  const hasPreview = Boolean(sandbox.preview_url_template) && running

  const timeLeft = !destroyed ? formatCountdown(sandbox.expires_at, now) : '—'
  const expired = !destroyed && new Date(sandbox.expires_at).getTime() <= now

  const openPort = (port: number) => {
    if (!sandbox.preview_url_template || port < 1 || port > 65535) return
    const url = sandbox.preview_url_template.replace('{port}', String(port))
    window.open(url, '_blank', 'noopener,noreferrer')
  }

  const customPortValid = useMemo(() => {
    if (!/^\d+$/.test(customPort)) return false
    const n = Number(customPort)
    return n >= 1 && n <= 65535
  }, [customPort])

  // Whole card is clickable so rows behave like links, but we stop the
  // click propagation on every interactive control below. Background
  // click → detail; button click → that button's action only.
  const goToDetail = () => navigate(`/sandboxes/${sandbox.id}`)
  const stop = (e: React.MouseEvent) => e.stopPropagation()

  return (
    <Card
      className={`cursor-pointer transition-colors hover:bg-muted/50 ${
        expired ? 'border-destructive/40' : ''
      }`}
      onClick={goToDetail}
    >
      <CardContent className="py-4 space-y-3">
        {/* Identity row + action cluster */}
        <div className="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
          <div className="min-w-0 space-y-1">
            <div className="flex items-center gap-2 flex-wrap">
              <Link
                to={`/sandboxes/${sandbox.id}`}
                onClick={stop}
                className="font-semibold leading-none hover:underline truncate"
              >
                {sandbox.name}
              </Link>
              <Badge variant={statusVariant(sandbox.status)}>
                {sandbox.status}
              </Badge>
              {sandbox.image && (
                <span className="font-mono text-xs text-muted-foreground truncate">
                  {sandbox.image}
                </span>
              )}
            </div>
            <div
              className="flex items-center gap-2 text-xs font-mono text-muted-foreground"
              onClick={stop}
            >
              <span className="truncate">{sandbox.id}</span>
              <CopyButton
                value={sandbox.id}
                minimal
                className="h-5 w-5 shrink-0"
              />
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2" onClick={stop}>
            {hasPreview && (
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button size="sm" className="gap-1">
                    <ExternalLink className="h-4 w-4" />
                    Open preview
                    <ChevronDown className="h-3 w-3 opacity-70" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-56">
                  {DEFAULT_PORTS.map((p) => (
                    <DropdownMenuItem
                      key={p.port}
                      onClick={() => openPort(p.port)}
                      className="justify-between"
                    >
                      <span className="font-mono text-xs">:{p.port}</span>
                      <span className="text-muted-foreground text-xs">
                        {p.label}
                      </span>
                    </DropdownMenuItem>
                  ))}
                  <DropdownMenuSeparator />
                  <div className="p-2 space-y-1.5">
                    <Label
                      htmlFor={`port-${sandbox.id}`}
                      className="text-[11px] text-muted-foreground"
                    >
                      Custom port
                    </Label>
                    <form
                      className="flex items-center gap-1.5"
                      onSubmit={(e) => {
                        e.preventDefault()
                        if (customPortValid) openPort(Number(customPort))
                      }}
                    >
                      <Input
                        id={`port-${sandbox.id}`}
                        type="number"
                        min={1}
                        max={65535}
                        placeholder="4321"
                        value={customPort}
                        onChange={(e) => setCustomPort(e.target.value)}
                        className="h-8 text-xs"
                      />
                      <Button
                        type="submit"
                        size="sm"
                        variant="outline"
                        className="h-8"
                        disabled={!customPortValid}
                      >
                        Go
                      </Button>
                    </form>
                  </div>
                </DropdownMenuContent>
              </DropdownMenu>
            )}

            {running && (
              <Button
                variant="outline"
                size="sm"
                onClick={() =>
                  pauseMutation.mutate({ path: { id: sandbox.id } })
                }
                disabled={pauseMutation.isPending}
              >
                <Square className="mr-1 h-4 w-4" />
                Stop
              </Button>
            )}
            {stopped && (
              <Button
                variant="outline"
                size="sm"
                onClick={() =>
                  resumeMutation.mutate({ path: { id: sandbox.id } })
                }
                disabled={resumeMutation.isPending}
              >
                <Play className="mr-1 h-4 w-4" />
                Resume
              </Button>
            )}
            {running && (
              <Button
                variant="outline"
                size="sm"
                onClick={() =>
                  restartMutation.mutate({ path: { id: sandbox.id } })
                }
                disabled={restartMutation.isPending}
                title="Restart"
              >
                <RotateCw className="h-4 w-4" />
              </Button>
            )}
            {!destroyed && (
              <Button
                variant="outline"
                size="sm"
                onClick={() => onDeleteRequest(sandbox)}
                className="text-destructive hover:text-destructive"
                title="Delete"
              >
                <Trash2 className="h-4 w-4" />
              </Button>
            )}
          </div>
        </div>

        {/* Live countdown + inline extend — hidden for destroyed rows */}
        {!destroyed && (
          <div
            className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between pt-1 border-t"
            onClick={stop}
          >
            <div className="flex items-center gap-4 flex-wrap pt-2">
              <div className="flex items-center gap-2">
                <Timer
                  className={`h-4 w-4 ${
                    expired ? 'text-destructive' : 'text-muted-foreground'
                  }`}
                />
                <div className="leading-tight">
                  <div
                    className={`font-mono text-xs tabular-nums ${
                      expired ? 'text-destructive' : ''
                    }`}
                  >
                    {expired ? 'expired' : `${timeLeft} left`}
                  </div>
                  <div className="text-xs text-muted-foreground">
                    created {formatAge(sandbox.created_at, now)}
                  </div>
                </div>
              </div>
            </div>
            <div className="flex items-center gap-1.5 pt-2 sm:pt-0">
              <span className="text-xs text-muted-foreground mr-1">
                Extend:
              </span>
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                onClick={() =>
                  extendMutation.mutate({
                    path: { id: sandbox.id },
                    body: { extra_secs: 900 },
                  })
                }
                disabled={extendMutation.isPending}
              >
                +15m
              </Button>
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                onClick={() =>
                  extendMutation.mutate({
                    path: { id: sandbox.id },
                    body: { extra_secs: 3600 },
                  })
                }
                disabled={extendMutation.isPending}
              >
                +1h
              </Button>
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                onClick={() =>
                  extendMutation.mutate({
                    path: { id: sandbox.id },
                    body: { extra_secs: 14400 },
                  })
                }
                disabled={extendMutation.isPending}
              >
                +4h
              </Button>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
