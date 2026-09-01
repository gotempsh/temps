// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
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
import {
  getTraefikDiscoveryStatusOptions,
  getTraefikDiscoveryStatusQueryKey,
  listTraefikDiscoveredRoutesOptions,
  listTraefikDiscoveredRoutesQueryKey,
  setTraefikDiscoveredRouteEnabledMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  TraefikDiscoveredRouteResponse,
  TraefikDiscoveryConflictResponse,
  TraefikDiscoveryStatusResponse,
  TraefikRouteTlsBlock,
} from '@/api/client/types.gen'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import {
  AlertCircle,
  AlertTriangle,
  CheckCircle2,
  Waypoints,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'

const PAGE_SIZE = 20

/**
 * A label an operator can paste onto a container they already run. Shown in the
 * onboarding state so "what would this do for me?" has a concrete answer rather
 * than an abstract description of label discovery.
 */
const EXAMPLE_LABELS = [
  'traefik.enable=true',
  'traefik.http.routers.myapp.rule=Host(`app.example.com`)',
  'traefik.http.services.myapp.loadbalancer.server.port=8080',
].join('\n')

/** Machine-readable conflict discriminators from the discovery service. */
const CONFLICT_REASONS: Record<string, string> = {
  owned_by_temps_route: 'Host already owned by Temps',
  claimed_by_another_container: 'Claimed by another container',
}

function humanizeConflictReason(reason: string): string {
  return CONFLICT_REASONS[reason] ?? reason.replace(/_/g, ' ')
}

function formatRelative(iso: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  return formatDistanceToNow(date, { addSuffix: true })
}

/**
 * Render the TLS status badge for a discovered route.
 *
 * - No `tls` label → plain dash.
 * - `tls` label, no cert row yet → amber "HTTPS – no cert" warning with both
 *   ADR-041 remedies surfaced as copyable CLI commands (Path A: request
 *   issuance, Path B: import an existing cert) — a route this far along
 *   already has an operator looking right at it, so the fix belongs here,
 *   not just in a doc.
 * - Cert row exists, drift detected → red "Drift" critical badge.
 * - Cert row exists, authorized, no drift → green "Authorized" badge.
 * - Cert row exists, not yet authorized → yellow "Pending" badge.
 */
function TlsStatusCell({
  host,
  tls,
  cert,
}: {
  host: string
  tls: boolean
  cert?: TraefikRouteTlsBlock | null
}) {
  if (!tls) {
    return <span className="text-xs text-muted-foreground">—</span>
  }
  if (!cert) {
    const requestCommand = `bunx @temps-sdk/cli traefik-discovery tls request ${host} --challenge-type http-01`
    const importCommand = `bunx @temps-sdk/cli traefik-discovery tls import acme.json --hosts ${host}`
    return (
      <span className="flex max-w-[280px] flex-col gap-1">
        <Badge variant="outline" className="w-fit border-amber-500 text-xs text-amber-600 dark:text-amber-400">
          TLS
        </Badge>
        <span className="text-xs text-amber-600 dark:text-amber-400">
          HTTPS will fail — no cert authorized
        </span>
        <span className="flex items-start justify-between gap-1 rounded-md bg-muted px-1.5 py-1">
          <code className="overflow-x-auto whitespace-nowrap font-mono text-[11px] leading-relaxed">
            {requestCommand}
          </code>
          <CopyButton
            value={requestCommand}
            minimal
            label="Copy the request-certificate command"
            className="shrink-0 rounded p-0.5"
          />
        </span>
        <span className="text-xs text-muted-foreground">
          Already have a cert from Traefik?{' '}
          <span className="inline-flex items-center gap-1">
            <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px]">
              {importCommand}
            </code>
            <CopyButton
              value={importCommand}
              minimal
              label="Copy the import-certificate command"
              className="shrink-0 rounded p-0.5"
            />
          </span>
        </span>
      </span>
    )
  }
  if (cert.container_drift) {
    return (
      <span className="flex flex-col gap-0.5">
        <Badge variant="destructive" className="w-fit text-xs">
          Drift
        </Badge>
        {cert.current_container_name && (
          <span className="text-xs text-muted-foreground">
            Now: {cert.current_container_name}
          </span>
        )}
      </span>
    )
  }
  if (cert.cert_authorized) {
    return (
      <Badge variant="outline" className="w-fit border-green-500 text-xs text-green-600 dark:text-green-400">
        Authorized
      </Badge>
    )
  }
  return (
    <Badge variant="secondary" className="w-fit text-xs">
      Pending
    </Badge>
  )
}

/**
 * Surface the RFC 7807 `detail` when the server sent one — a bare "Request
 * failed" leaves a self-hosted operator with nothing to act on.
 */
function describeError(error: unknown): string | undefined {
  if (error && typeof error === 'object') {
    const problem = error as { detail?: unknown; title?: unknown }
    if (typeof problem.detail === 'string') return problem.detail
    if (typeof problem.title === 'string') return problem.title
  }
  if (error instanceof Error && error.message) return error.message
  return undefined
}

// ---------------------------------------------------------------------------
// Status card
// ---------------------------------------------------------------------------

function StatTile({
  label,
  value,
  description,
}: {
  label: string
  value: number
  description?: string
}) {
  return (
    <div className="rounded-lg border bg-card p-4">
      <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </p>
      <p className="mt-1 text-2xl font-semibold tabular-nums">
        {value.toLocaleString()}
      </p>
      {description && (
        <p className="mt-1 text-xs text-muted-foreground">{description}</p>
      )}
    </div>
  )
}

function ConflictList({
  conflicts,
}: {
  conflicts: TraefikDiscoveryConflictResponse[]
}) {
  return (
    <Alert className="mt-4 border-amber-400">
      <AlertTriangle className="h-4 w-4 text-amber-500" />
      <AlertTitle>
        {conflicts.length.toLocaleString()} labelled container
        {conflicts.length === 1 ? '' : 's'} not adopted
      </AlertTitle>
      <AlertDescription>
        <p className="mb-2">
          These containers carry Traefik labels but were deliberately skipped,
          so their hostnames are not being served from them.
        </p>
        <ul className="space-y-1.5">
          {conflicts.map((conflict) => (
            <li
              key={`${conflict.host}:${conflict.container_id}`}
              className="text-xs"
            >
              <span className="font-mono font-medium">{conflict.host}</span>
              <span className="text-muted-foreground">
                {' '}
                — {humanizeConflictReason(conflict.reason)}
              </span>
              <span className="block text-muted-foreground">
                {conflict.container_name}: {conflict.detail}
              </span>
            </li>
          ))}
        </ul>
      </AlertDescription>
    </Alert>
  )
}

function SetupInstructions({
  status,
}: {
  status: TraefikDiscoveryStatusResponse
}) {
  return (
    <div className="space-y-4">
      <div className="rounded-lg border border-dashed p-4">
        <p className="text-sm font-medium">What this would do</p>
        <p className="mt-1 text-sm text-muted-foreground">
          A container you already run — from docker-compose, Coolify, Dokploy or
          anything else — carrying a{' '}
          <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">
            Host(`app.example.com`)
          </code>{' '}
          Traefik label becomes a working route on this Temps instance, with no
          change to that container. Temps reads the labels; it never rewrites
          them, and it never displaces a deployment, custom domain or the
          console hostname.
        </p>
        <div className="mt-3 flex items-start justify-between gap-2 rounded-md bg-muted p-3">
          <pre className="overflow-x-auto text-xs leading-relaxed">
            <code>{EXAMPLE_LABELS}</code>
          </pre>
          <CopyButton
            value={EXAMPLE_LABELS}
            minimal
            label="Copy example labels"
            className="shrink-0 rounded p-1"
          />
        </div>
      </div>

      <Alert>
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Discovery is not running on this server</AlertTitle>
        <AlertDescription>
          {status.reason ??
            `${status.setup.enable_env_var} is not set, so no containers are being watched.`}
        </AlertDescription>
      </Alert>

      <div>
        <p className="text-sm font-medium">How to turn it on</p>
        <p className="mt-1 text-sm text-muted-foreground">
          Which containers this machine may adopt is operator policy, so this is
          not configurable from the console — it is read from the server&apos;s
          environment at process start. Set these variables where{' '}
          <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">
            temps serve
          </code>{' '}
          is launched
          {status.setup.requires_restart ? ' and restart the server' : ''}.
        </p>

        <dl className="mt-3 space-y-2 text-sm">
          <div className="flex flex-col gap-0.5 sm:flex-row sm:gap-2">
            <dt className="font-mono text-xs sm:w-[280px] sm:shrink-0">
              {status.setup.enable_env_var}
            </dt>
            <dd className="text-xs text-muted-foreground">
              Set to <code className="font-mono">true</code> to opt this
              installation in.
            </dd>
          </div>
          <div className="flex flex-col gap-0.5 sm:flex-row sm:gap-2">
            <dt className="font-mono text-xs sm:w-[280px] sm:shrink-0">
              {status.setup.network_env_var}
            </dt>
            <dd className="text-xs text-muted-foreground">
              Docker network to watch. Currently{' '}
              <code className="font-mono">{status.network}</code>.
            </dd>
          </div>
        </dl>

        <div className="mt-3 flex items-start justify-between gap-2 rounded-md bg-muted p-3">
          <pre className="overflow-x-auto text-xs leading-relaxed">
            <code>{status.setup.example}</code>
          </pre>
          <CopyButton
            value={status.setup.example}
            minimal
            label="Copy the enable command"
            className="shrink-0 rounded p-1"
          />
        </div>

        {status.setup.requires_restart && (
          <p className="mt-2 text-xs text-muted-foreground">
            These are read once at process start — changing them requires
            restarting the server.
          </p>
        )}
      </div>

      {status.discovered_route_count > 0 && (
        <Alert>
          <AlertTriangle className="h-4 w-4 text-amber-500" />
          <AlertTitle>
            {status.discovered_route_count.toLocaleString()} previously
            discovered route
            {status.discovered_route_count === 1 ? '' : 's'} still recorded
          </AlertTitle>
          <AlertDescription>
            Discovery ran on this instance before. Those rows are kept, but
            nothing is being served from them while discovery is off.
          </AlertDescription>
        </Alert>
      )}
    </div>
  )
}

function RunningStatus({ status }: { status: TraefikDiscoveryStatusResponse }) {
  const reconciliation = status.last_reconciliation
  const conflicts = reconciliation?.conflicts ?? []

  return (
    <div className="space-y-4">
      <dl className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <div className="rounded-lg border bg-card p-4">
          <dt className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            Watched network
          </dt>
          <dd className="mt-1 font-mono text-sm">{status.network}</dd>
        </div>
        <div className="rounded-lg border bg-card p-4">
          <dt className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            Poll interval
          </dt>
          <dd className="mt-1 text-sm">
            Every {status.poll_interval_seconds.toLocaleString()}&nbsp;s
          </dd>
        </div>
      </dl>

      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        <StatTile
          label="Discovered"
          value={status.discovered_route_count}
          description="Rows found by discovery"
        />
        <StatTile
          label="Enabled"
          value={status.enabled_route_count}
          description="In the live route table"
        />
        {reconciliation && (
          <>
            <StatTile
              label="Containers scanned"
              value={reconciliation.containers_scanned}
              description="Last reconciliation"
            />
            <StatTile
              label="Temps-managed"
              value={reconciliation.skipped_temps_managed}
              description="Skipped, already routed"
            />
          </>
        )}
      </div>

      {reconciliation ? (
        <div className="rounded-lg border p-4">
          <div className="flex flex-col gap-1 sm:flex-row sm:items-center sm:justify-between">
            <p className="text-sm font-medium">Last reconciliation</p>
            <p className="text-xs text-muted-foreground">
              {formatRelative(reconciliation.completed_at)} on{' '}
              <span className="font-mono">{reconciliation.network}</span>
            </p>
          </div>
          <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
            <span>
              Upserted:{' '}
              <span className="tabular-nums text-foreground">
                {reconciliation.routes_upserted.toLocaleString()}
              </span>
            </span>
            <span>
              Unchanged:{' '}
              <span className="tabular-nums text-foreground">
                {reconciliation.routes_unchanged.toLocaleString()}
              </span>
            </span>
            <span>
              Removed:{' '}
              <span className="tabular-nums text-foreground">
                {reconciliation.routes_removed.toLocaleString()}
              </span>
            </span>
            <span>
              Conflicts:{' '}
              <span
                className={`tabular-nums ${conflicts.length > 0 ? 'text-amber-600 dark:text-amber-400' : 'text-foreground'}`}
              >
                {conflicts.length.toLocaleString()}
              </span>
            </span>
          </div>
        </div>
      ) : (
        <div className="flex items-center gap-2 rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
          <AlertCircle className="h-4 w-4" />
          Discovery is running but has not completed a reconciliation pass yet.
        </div>
      )}

      {conflicts.length > 0 && <ConflictList conflicts={conflicts} />}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Discovered routes table
// ---------------------------------------------------------------------------

function RouteRow({
  route,
  onToggle,
  isToggling,
}: {
  route: TraefikDiscoveredRouteResponse
  onToggle: (host: string, enabled: boolean) => void
  isToggling: boolean
}) {
  return (
    <TableRow>
      <TableCell>
        <span className="block font-mono text-sm font-medium">
          {route.host}
        </span>
        <span className="block text-xs text-muted-foreground md:hidden">
          {route.target_container_name}:{route.target_port}
        </span>
        {route.contested_by.length > 0 && (
          <span className="mt-0.5 block text-xs text-amber-600 dark:text-amber-400">
            Also claimed by {route.contested_by.join(', ')}
          </span>
        )}
      </TableCell>
      <TableCell className="hidden md:table-cell">
        <span className="block font-mono text-xs">
          {route.target_container_name}
        </span>
        <span className="block text-xs text-muted-foreground">
          router {route.router_name}
        </span>
      </TableCell>
      <TableCell className="hidden text-right tabular-nums sm:table-cell">
        {route.target_port}
        {route.target_host_port !== null &&
          route.target_host_port !== undefined && (
            <span className="block text-xs text-muted-foreground">
              host {route.target_host_port}
            </span>
          )}
      </TableCell>
      <TableCell className="hidden md:table-cell">
        <TlsStatusCell host={route.host} tls={route.tls} cert={route.tls_certificate} />
      </TableCell>
      <TableCell>
        {route.active ? (
          <Badge variant="outline" className="text-xs">
            Active
          </Badge>
        ) : (
          <span className="flex flex-col gap-0.5">
            <Badge variant="secondary" className="w-fit text-xs">
              Inactive
            </Badge>
            {route.inactive_reason && (
              <span className="text-xs text-muted-foreground">
                {route.inactive_reason}
              </span>
            )}
          </span>
        )}
      </TableCell>
      <TableCell className="text-right">
        <Switch
          checked={route.enabled}
          disabled={isToggling}
          onCheckedChange={(checked) => onToggle(route.host, checked)}
          aria-label={`${route.enabled ? 'Disable' : 'Enable'} routing for ${route.host}`}
        />
      </TableCell>
    </TableRow>
  )
}

function RoutesTableSkeleton() {
  return (
    <div className="space-y-2">
      <Skeleton className="h-9 w-full" />
      <Skeleton className="h-9 w-full" />
      <Skeleton className="h-9 w-full" />
    </div>
  )
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export function TraefikDiscoveryPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()
  const [page, setPage] = useState(1)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Traefik Discovery' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Traefik Discovery')

  const {
    data: status,
    isLoading: statusLoading,
    error: statusError,
  } = useQuery({
    ...getTraefikDiscoveryStatusOptions(),
    // The watcher reconciles on an interval, so an operator sitting on this
    // page after labelling a container sees it appear without a manual refresh.
    refetchInterval: 30_000,
  })

  const discoveryOn = status?.configured === true

  const {
    data: routesData,
    isLoading: routesLoading,
    error: routesError,
  } = useQuery({
    ...listTraefikDiscoveredRoutesOptions({
      query: { page, page_size: PAGE_SIZE },
    }),
    // An unconfigured install has nothing to list; don't spend a request on it.
    enabled: discoveryOn,
    refetchInterval: 30_000,
  })

  const toggleRoute = useMutation({
    ...setTraefikDiscoveredRouteEnabledMutation(),
    onSuccess: (_data, variables) => {
      const host = variables.path?.host ?? 'route'
      const enabled = variables.body?.enabled
      void queryClient.invalidateQueries({
        queryKey: listTraefikDiscoveredRoutesQueryKey(),
      })
      void queryClient.invalidateQueries({
        queryKey: getTraefikDiscoveryStatusQueryKey(),
      })
      toast.success(
        enabled
          ? `Routing enabled for ${host}`
          : `Routing disabled for ${host}`,
        {
          description: enabled
            ? 'The proxy reloads its route table automatically.'
            : 'The container keeps its labels; Temps just stops serving the host.',
        }
      )
    },
    onError: (error, variables) => {
      const host = variables.path?.host ?? 'route'
      toast.error(`Could not update ${host}`, {
        description:
          describeError(error) ??
          'The server rejected the change. Try again, or check the server logs.',
      })
    },
  })

  const pendingHost = toggleRoute.isPending
    ? toggleRoute.variables?.path?.host
    : undefined

  const handleToggle = (host: string, enabled: boolean) => {
    toggleRoute.mutate({ path: { host }, body: { enabled } })
  }

  const routes = routesData?.routes ?? []
  const total = routesData?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE))

  // Routes with active certificate drift (ADR-041 §8: persistent Critical banner).
  const driftRoutes = routes.filter((r) => r.tls_certificate?.container_drift)

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-1">
        <div className="flex items-center gap-2">
          <Waypoints className="h-5 w-5 text-muted-foreground" />
          <h1 className="text-xl font-semibold">Traefik Discovery</h1>
        </div>
        <p className="text-sm text-muted-foreground">
          Route containers Temps did not deploy by reading the Traefik labels
          they already carry, so an existing docker-compose, Coolify or Dokploy
          stack becomes reachable through this instance without changing those
          containers.
        </p>
      </div>

      {/* Status — always rendered. When discovery is off this is the onboarding
          surface, not an empty page: the feature has to explain itself. */}
      <Card>
        <CardHeader className="pb-3">
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
            <div>
              <CardTitle className="text-base">Status</CardTitle>
              <CardDescription>
                Whether this server is watching a Docker network for Traefik
                labels.
              </CardDescription>
            </div>
            {!statusLoading && status && (
              <Badge
                variant={status.configured ? 'outline' : 'secondary'}
                className="w-fit"
              >
                {status.configured ? (
                  <>
                    <CheckCircle2 className="mr-1 h-3 w-3 text-green-600 dark:text-green-500" />
                    Running
                  </>
                ) : (
                  'Not enabled'
                )}
              </Badge>
            )}
          </div>
        </CardHeader>
        <CardContent>
          {statusError ? (
            <Alert variant="destructive">
              <AlertCircle className="h-4 w-4" />
              <AlertTitle>Failed to load discovery status</AlertTitle>
              <AlertDescription>
                {describeError(statusError) ??
                  'Could not fetch Traefik discovery status. The server may be unavailable, or you may not have permission to view it.'}
              </AlertDescription>
            </Alert>
          ) : statusLoading || !status ? (
            <div className="space-y-3">
              <Skeleton className="h-20 w-full" />
              <Skeleton className="h-24 w-full" />
            </div>
          ) : status.configured ? (
            <RunningStatus status={status} />
          ) : (
            <SetupInstructions status={status} />
          )}
        </CardContent>
      </Card>

      {/* Nothing to list until discovery actually runs. */}
      {discoveryOn && (
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-base">Discovered routes</CardTitle>
            <CardDescription>
              Containers adopted from their Traefik labels. Disabling a route
              stops Temps serving that host without touching the
              container&apos;s labels, so the row stays visible.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            {routesError ? (
              <Alert variant="destructive">
                <AlertCircle className="h-4 w-4" />
                <AlertTitle>Failed to load discovered routes</AlertTitle>
                <AlertDescription>
                  {describeError(routesError) ??
                    'The route list could not be fetched. The status above is unaffected.'}
                </AlertDescription>
              </Alert>
            ) : routesLoading ? (
              <RoutesTableSkeleton />
            ) : routes.length === 0 ? (
              <div className="rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
                <p className="font-medium text-foreground">
                  No containers discovered yet
                </p>
                <p className="mt-1">
                  Nothing on{' '}
                  <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">
                    {status?.network}
                  </code>{' '}
                  carries a{' '}
                  <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">
                    traefik.enable=true
                  </code>{' '}
                  label that Temps can adopt. Add the labels below to a running
                  container and it appears here on the next reconciliation.
                </p>
                <div className="mt-3 flex items-start justify-between gap-2 rounded-md bg-muted p-3">
                  <pre className="overflow-x-auto text-xs leading-relaxed">
                    <code>{EXAMPLE_LABELS}</code>
                  </pre>
                  <CopyButton
                    value={EXAMPLE_LABELS}
                    minimal
                    label="Copy example labels"
                    className="shrink-0 rounded p-1"
                  />
                </div>
              </div>
            ) : (
              <>
                {driftRoutes.length > 0 && (
                  <Alert variant="destructive" className="mb-4">
                    <AlertCircle className="h-4 w-4" />
                    <AlertTitle>Certificate drift detected</AlertTitle>
                    <AlertDescription>
                      <p className="mb-1">
                        The following {driftRoutes.length === 1 ? 'host is' : 'hosts are'}{' '}
                        now served by a different container than the one that was
                        authorized for TLS. HTTPS will fail until the certificate
                        is re-authorized.
                      </p>
                      <ul className="list-disc pl-4 text-sm">
                        {driftRoutes.map((r) => (
                          <li key={r.host}>
                            <span className="font-mono">{r.host}</span>
                            {r.tls_certificate?.current_container_name && (
                              <span className="text-muted-foreground">
                                {' '}— now served by{' '}
                                <span className="font-mono">
                                  {r.tls_certificate.current_container_name}
                                </span>
                              </span>
                            )}
                          </li>
                        ))}
                      </ul>
                    </AlertDescription>
                  </Alert>
                )}
                <div className="overflow-x-auto">
                  <Table className="min-w-[560px]">
                    <TableHeader>
                      <TableRow>
                        <TableHead>Host</TableHead>
                        <TableHead className="hidden md:table-cell">
                          Container
                        </TableHead>
                        <TableHead className="hidden text-right sm:table-cell">
                          Port
                        </TableHead>
                        <TableHead className="hidden md:table-cell">
                          TLS
                        </TableHead>
                        <TableHead>State</TableHead>
                        <TableHead className="text-right">Enabled</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {routes.map((route) => (
                        <RouteRow
                          key={route.id}
                          route={route}
                          onToggle={handleToggle}
                          isToggling={pendingHost === route.host}
                        />
                      ))}
                    </TableBody>
                  </Table>
                </div>

                {totalPages > 1 && (
                  <ResponsivePagination
                    page={page}
                    pageSize={PAGE_SIZE}
                    total={total}
                    totalPages={totalPages}
                    ariaLabel="Discovered route pagination"
                    onPageChange={setPage}
                  />
                )}
              </>
            )}
          </CardContent>
        </Card>
      )}
    </div>
  )
}
