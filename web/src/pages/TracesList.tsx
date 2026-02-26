import {
  EnvironmentResponse,
  ProjectResponse,
  SpanStatusCode,
  TraceSummary,
} from '@/api/client'
import {
  getEnvironmentsOptions,
  getProjectDeploymentsOptions,
  queryTraceSummariesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CodeBlock } from '@/components/ui/code-block'
import {
  Collapsible,
  CollapsibleContent,
} from '@/components/ui/collapsible'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
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
import {
  AlertTriangle,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Clock,
  Code2,
  RefreshCw,
  Search,
  Settings2,
  Workflow,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'

interface TracesListProps {
  project: ProjectResponse
}

type TimeRange = '1h' | '6h' | '24h' | '7d' | '30d'

function statusBadge(status: SpanStatusCode) {
  switch (status) {
    case 'OK':
      return <Badge variant="success">OK</Badge>
    case 'ERROR':
      return <Badge variant="destructive">Error</Badge>
    default:
      return null
  }
}

function kindBadge(kind: string) {
  const colors: Record<string, string> = {
    SERVER: 'bg-blue-100 text-blue-800 dark:bg-blue-900/30 dark:text-blue-300',
    CLIENT:
      'bg-purple-100 text-purple-800 dark:bg-purple-900/30 dark:text-purple-300',
    PRODUCER:
      'bg-amber-100 text-amber-800 dark:bg-amber-900/30 dark:text-amber-300',
    CONSUMER:
      'bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-300',
    INTERNAL:
      'bg-gray-100 text-gray-800 dark:bg-gray-900/30 dark:text-gray-300',
  }
  return (
    <span
      className={`inline-flex items-center rounded px-1.5 py-0.5 text-xs font-medium ${colors[kind] || colors.INTERNAL}`}
    >
      {kind}
    </span>
  )
}

function formatDuration(ms: number): string {
  if (ms < 1) return '<1ms'
  if (ms < 1000) return `${Math.round(ms)}ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`
  return `${(ms / 60_000).toFixed(1)}m`
}

function durationColor(ms: number): string {
  if (ms < 100) return 'text-green-600 dark:text-green-400'
  if (ms < 500) return 'text-yellow-600 dark:text-yellow-400'
  if (ms < 2000) return 'text-orange-600 dark:text-orange-400'
  return 'text-red-600 dark:text-red-400'
}

const PAGE_SIZE = 10

// ── Setup Section ───────────────────────────────────────────────────

function OtelSetupSection({ project }: { project: ProjectResponse }) {
  const [selectedEnvId, setSelectedEnvId] = useState<string>('')

  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
    enabled: !!project.id,
  })

  // Auto-select first environment
  useEffect(() => {
    if (environments && environments.length > 0 && !selectedEnvId) {
      setSelectedEnvId(String(environments[0].id))
    }
  }, [environments, selectedEnvId])

  const selectedEnv = useMemo(
    () => environments?.find((e) => String(e.id) === selectedEnvId),
    [environments, selectedEnvId]
  )

  const baseUrl = window.location.origin
  // Path-based endpoint: /api/otel/v1/{project_id}/{environment_id}/{deployment_id}
  // Use 0 as deployment_id placeholder — it will be set per-deployment at runtime
  const otlpEndpoint = selectedEnv
    ? `${baseUrl}/api/otel/v1/${project.id}/${selectedEnv.id}/0`
    : `${baseUrl}/api/otel/v1/${project.id}/0/0`

  const nextjsSetupCode = `// instrumentation.ts (project root)
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-proto'
import { NodeSDK } from '@opentelemetry/sdk-node'
import { BatchSpanProcessor } from '@opentelemetry/sdk-trace-node'

export function register() {
  // OTEL_EXPORTER_OTLP_ENDPOINT, OTEL_SERVICE_NAME, and auth headers
  // are auto-configured via environment variables on Temps deployments.
  const sdk = new NodeSDK({
    spanProcessors: [new BatchSpanProcessor(new OTLPTraceExporter())],
  })

  sdk.start()
}`

  const nextConfigCode = `// next.config.js
/** @type {import('next').NextConfig} */
const nextConfig = {
  experimental: {
    instrumentationHook: true,
  },
}

module.exports = nextConfig`

  const envVarsCode = `# These are auto-injected on Temps deployments.
# Only set manually for external hosting (Vercel, Netlify, etc.).
OTEL_EXPORTER_OTLP_ENDPOINT=${otlpEndpoint}
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <YOUR_API_KEY>"
OTEL_SERVICE_NAME=${project.name}`

  const installCmd =
    'npm install @opentelemetry/sdk-node @opentelemetry/sdk-trace-node @opentelemetry/exporter-trace-otlp-proto'

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <Code2 className="h-4 w-4" />
          Setup OpenTelemetry
        </CardTitle>
        <CardDescription>
          Apps deployed on Temps get OpenTelemetry environment variables
          automatically. Just add the SDK to start sending traces.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        {/* Auto-injected note */}
        <div className="rounded-md border border-green-200 bg-green-50 dark:border-green-900/50 dark:bg-green-900/10 p-3">
          <p className="text-xs text-green-800 dark:text-green-200">
            <strong>Deployed on Temps?</strong> The OTLP endpoint, auth token,
            service name, and version are injected automatically on every
            deployment. You only need to install the SDK and create the
            instrumentation file below.
          </p>
        </div>

        {/* Step 1: Install */}
        <div className="space-y-2">
          <h4 className="text-sm font-medium">1. Install dependencies</h4>
          <CodeBlock code={installCmd} language="bash" />
        </div>

        {/* Step 2: Create instrumentation.ts */}
        <div className="space-y-2">
          <h4 className="text-sm font-medium">
            2. Create <code>instrumentation.ts</code>
          </h4>
          <CodeBlock
            code={nextjsSetupCode}
            language="typescript"
            title="instrumentation.ts"
          />
        </div>

        {/* Step 3: Enable in next.config.js */}
        <div className="space-y-2">
          <h4 className="text-sm font-medium">
            3. Enable instrumentation hook
          </h4>
          <CodeBlock
            code={nextConfigCode}
            language="javascript"
            title="next.config.js"
          />
        </div>

        {/* External hosting: manual env vars */}
        <div className="space-y-2">
          <h4 className="text-sm font-medium">
            External hosting (Vercel, Netlify, etc.)
          </h4>
          <p className="text-xs text-muted-foreground">
            If your app is <strong>not</strong> deployed on Temps, set these
            environment variables manually:
          </p>

          {/* Environment selector for endpoint */}
          <div className="flex items-center gap-3">
            <span className="text-xs text-muted-foreground shrink-0">
              Environment:
            </span>
            <Select value={selectedEnvId} onValueChange={setSelectedEnvId}>
              <SelectTrigger className="w-[200px] h-8">
                <SelectValue placeholder="Select environment" />
              </SelectTrigger>
              <SelectContent>
                {environments?.map((env: EnvironmentResponse) => (
                  <SelectItem key={env.id} value={String(env.id)}>
                    {env.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <CodeBlock code={envVarsCode} language="bash" title=".env" />
          <p className="text-xs text-muted-foreground">
            Replace <code>&lt;YOUR_API_KEY&gt;</code> with a Temps API key (
            <code>tk_...</code>) from{' '}
            <strong>Settings &rarr; API Keys</strong>.
          </p>
        </div>
      </CardContent>
    </Card>
  )
}

// ── Main Component ──────────────────────────────────────────────────

export default function TracesList({ project }: TracesListProps) {
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle(`Traces - ${project.name}`)
  const [showSetup, setShowSetup] = useState(false)

  // State from URL params
  const [timeRange, setTimeRange] = useState<TimeRange>(
    () => (searchParams.get('range') as TimeRange) || '24h'
  )
  const [serviceName, setServiceName] = useState(
    () => searchParams.get('service') || ''
  )
  const [status, setStatus] = useState(
    () => searchParams.get('status') || 'all'
  )
  const [search, setSearch] = useState(
    () => searchParams.get('q') || ''
  )
  const [environmentId, setEnvironmentId] = useState(
    () => searchParams.get('env') || 'all'
  )
  const [deploymentId, setDeploymentId] = useState(
    () => searchParams.get('deploy') || 'all'
  )
  const [page, setPage] = useState(() => {
    const p = searchParams.get('page')
    return p ? parseInt(p, 10) : 1
  })

  // Compute time window
  const { startTime, endTime } = useMemo(() => {
    const now = new Date()
    const start = new Date()
    switch (timeRange) {
      case '1h':
        start.setHours(start.getHours() - 1)
        break
      case '6h':
        start.setHours(start.getHours() - 6)
        break
      case '24h':
        start.setDate(start.getDate() - 1)
        break
      case '7d':
        start.setDate(start.getDate() - 7)
        break
      case '30d':
        start.setDate(start.getDate() - 30)
        break
    }
    return { startTime: start.toISOString(), endTime: now.toISOString() }
  }, [timeRange])

  // Fetch environments for the filter dropdown
  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
    enabled: !!project.id,
  })

  // Fetch deployments for the selected environment (or all)
  const { data: deploymentsData } = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: project.id },
      query: {
        environment_id:
          environmentId !== 'all' ? Number(environmentId) : undefined,
        per_page: 50,
      },
    }),
    enabled: !!project.id,
  })

  const deployments = deploymentsData?.deployments

  // Sync state to URL
  useEffect(() => {
    const params = new URLSearchParams()
    if (timeRange !== '24h') params.set('range', timeRange)
    if (serviceName) params.set('service', serviceName)
    if (status !== 'all') params.set('status', status)
    if (search) params.set('q', search)
    if (environmentId !== 'all') params.set('env', environmentId)
    if (deploymentId !== 'all') params.set('deploy', deploymentId)
    if (page > 1) params.set('page', page.toString())
    setSearchParams(params, { replace: true })
  }, [timeRange, serviceName, status, search, environmentId, deploymentId, page, setSearchParams])

  // Breadcrumbs
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project.name, href: `/projects/${project.slug}` },
      { label: 'Traces' },
    ])
  }, [project.name, project.slug, setBreadcrumbs])

  // Fetch trace summaries (one row per trace, server-side aggregation)
  const { data, isLoading, isFetching, refetch } = useQuery({
    ...queryTraceSummariesOptions({
      query: {
        project_id: project.id,
        start_time: startTime,
        end_time: endTime,
        service_name: serviceName || undefined,
        status: status !== 'all' ? status : undefined,
        trace_id: search || undefined,
        environment_id:
          environmentId !== 'all' ? Number(environmentId) : undefined,
        deployment_id:
          deploymentId !== 'all' ? Number(deploymentId) : undefined,
        limit: PAGE_SIZE,
        offset: (page - 1) * PAGE_SIZE,
      },
    }),
  })

  const traces: TraceSummary[] = data?.data ?? []
  const totalCount = data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(totalCount / PAGE_SIZE))

  // Extract unique service names for the filter dropdown
  const serviceNames = useMemo(() => {
    if (!traces.length) return []
    const names = new Set(traces.map((t) => t.service_name))
    return Array.from(names).sort()
  }, [traces])

  const handleTimeRangeChange = useCallback(
    (v: string) => {
      setTimeRange(v as TimeRange)
      setPage(1)
    },
    []
  )
  const handleStatusChange = useCallback(
    (v: string) => {
      setStatus(v)
      setPage(1)
    },
    []
  )
  const handleServiceChange = useCallback(
    (v: string) => {
      setServiceName(v === '__all__' ? '' : v)
      setPage(1)
    },
    []
  )
  const handleEnvironmentChange = useCallback(
    (v: string) => {
      setEnvironmentId(v)
      setDeploymentId('all') // Reset deployment when environment changes
      setPage(1)
    },
    []
  )
  const handleDeploymentChange = useCallback(
    (v: string) => {
      setDeploymentId(v)
      setPage(1)
    },
    []
  )

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">Traces</h2>
          <p className="text-sm text-muted-foreground">
            Distributed traces from your application via OpenTelemetry
          </p>
        </div>
        <div className="flex items-center gap-3">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => refetch()}
            disabled={isFetching}
          >
            <RefreshCw className={`h-4 w-4 ${isFetching ? 'animate-spin' : ''}`} />
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setShowSetup((v) => !v)}
            className="gap-2"
          >
            <Settings2 className="h-3.5 w-3.5" />
            <span className="hidden sm:inline">Setup</span>
            <ChevronDown
              className={`h-3.5 w-3.5 transition-transform ${showSetup ? 'rotate-180' : ''}`}
            />
          </Button>
          {totalCount > 0 && (
            <span className="text-sm text-muted-foreground">
              {totalCount.toLocaleString()} trace{totalCount !== 1 ? 's' : ''}
            </span>
          )}
        </div>
      </div>

      {/* Setup section */}
      <Collapsible open={showSetup} onOpenChange={setShowSetup}>
        <CollapsibleContent>
          <OtelSetupSection project={project} />
        </CollapsibleContent>
      </Collapsible>

      {/* Filters */}
      <Card>
        <CardContent className="p-3">
          <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
            <Select value={timeRange} onValueChange={handleTimeRangeChange}>
              <SelectTrigger className="w-full sm:w-[140px]">
                <Clock className="mr-2 h-3.5 w-3.5" />
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="1h">Last 1 hour</SelectItem>
                <SelectItem value="6h">Last 6 hours</SelectItem>
                <SelectItem value="24h">Last 24 hours</SelectItem>
                <SelectItem value="7d">Last 7 days</SelectItem>
                <SelectItem value="30d">Last 30 days</SelectItem>
              </SelectContent>
            </Select>

            <Select value={status} onValueChange={handleStatusChange}>
              <SelectTrigger className="w-full sm:w-[120px]">
                <SelectValue placeholder="Status" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All Status</SelectItem>
                <SelectItem value="OK">OK</SelectItem>
                <SelectItem value="ERROR">Error</SelectItem>
              </SelectContent>
            </Select>

            {environments && environments.length > 0 && (
              <Select
                value={environmentId}
                onValueChange={handleEnvironmentChange}
              >
                <SelectTrigger className="w-full sm:w-[180px]">
                  <SelectValue placeholder="Environment" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All Environments</SelectItem>
                  {environments.map((env: EnvironmentResponse) => (
                    <SelectItem key={env.id} value={String(env.id)}>
                      {env.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}

            {deployments && deployments.length > 0 && (
              <Select
                value={deploymentId}
                onValueChange={handleDeploymentChange}
              >
                <SelectTrigger className="w-full sm:w-[180px]">
                  <SelectValue placeholder="Deployment" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All Deployments</SelectItem>
                  {deployments.map((d) => (
                    <SelectItem key={d.id} value={String(d.id)}>
                      #{d.id}{d.slug ? ` (${d.slug})` : ''}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}

            {serviceNames.length > 0 && (
              <Select
                value={serviceName || '__all__'}
                onValueChange={handleServiceChange}
              >
                <SelectTrigger className="w-full sm:w-[180px]">
                  <SelectValue placeholder="Service" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">All Services</SelectItem>
                  {serviceNames.map((name) => (
                    <SelectItem key={name} value={name}>
                      {name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}

            <div className="relative flex-1 min-w-0 sm:min-w-[200px]">
              <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                placeholder="Search by trace ID..."
                value={search}
                onChange={(e) => {
                  setSearch(e.target.value)
                  setPage(1)
                }}
                className="pl-8 h-9"
              />
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Table */}
      {isLoading ? (
        <div className="space-y-2">
          {Array.from({ length: 8 }).map((_, i) => (
            <Skeleton key={`skel-${i}`} className="h-12 w-full" />
          ))}
        </div>
      ) : traces.length === 0 ? (
        <EmptyState
          icon={Workflow}
          title="No traces found"
          description={
            search || serviceName || status !== 'all' || environmentId !== 'all' || deploymentId !== 'all'
              ? 'Try adjusting your filters or time range.'
              : 'Traces will appear here once your application sends data via OpenTelemetry.'
          }
          action={
            !search && !serviceName && status === 'all' && environmentId === 'all' && deploymentId === 'all' && !showSetup ? (
              <Button
                variant="outline"
                size="sm"
                className="gap-2"
                onClick={() => setShowSetup(true)}
              >
                <Settings2 className="h-3.5 w-3.5" />
                View setup instructions
              </Button>
            ) : undefined
          }
        />
      ) : (
        <>
          <div className="rounded-md border overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="min-w-[200px] md:w-[300px]">Trace</TableHead>
                  <TableHead>Service</TableHead>
                  {environmentId === 'all' && <TableHead className="hidden lg:table-cell">Environment</TableHead>}
                  <TableHead className="hidden md:table-cell">Kind</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="text-right">Duration</TableHead>
                  <TableHead className="hidden md:table-cell text-right">Spans</TableHead>
                  <TableHead className="hidden md:table-cell text-right">Timestamp</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {traces.map((trace) => (
                  <TableRow
                    key={trace.trace_id}
                    className="cursor-pointer hover:bg-muted/50"
                    onClick={() => navigate(trace.trace_id)}
                  >
                    <TableCell>
                      <div className="flex flex-col gap-0.5">
                        <span className="font-medium truncate max-w-[200px] md:max-w-[280px]">
                          {trace.root_span_name}
                        </span>
                        <span className="text-xs text-muted-foreground font-mono truncate max-w-[200px] md:max-w-[280px]">
                          {trace.trace_id.slice(0, 16)}...
                        </span>
                      </div>
                    </TableCell>
                    <TableCell>
                      <span className="text-sm">
                        {trace.service_name}
                      </span>
                    </TableCell>
                    {environmentId === 'all' && (
                      <TableCell className="hidden lg:table-cell">
                        {trace.deployment_environment ? (
                          <Badge variant="secondary" className="font-normal">
                            {trace.deployment_environment}
                          </Badge>
                        ) : (
                          <span className="text-xs text-muted-foreground">—</span>
                        )}
                      </TableCell>
                    )}
                    <TableCell className="hidden md:table-cell">{kindBadge(trace.kind)}</TableCell>
                    <TableCell>
                      <div className="flex items-center gap-1.5">
                        {trace.error_count > 0
                          ? statusBadge('ERROR')
                          : statusBadge('OK')}
                        {trace.error_count > 0 && (
                          <span className="flex items-center text-xs text-destructive">
                            <AlertTriangle className="mr-0.5 h-3 w-3" />
                            {trace.error_count}
                          </span>
                        )}
                      </div>
                    </TableCell>
                    <TableCell className="text-right">
                      <span
                        className={`font-mono text-sm ${durationColor(trace.duration_ms)}`}
                      >
                        {formatDuration(trace.duration_ms)}
                      </span>
                    </TableCell>
                    <TableCell className="hidden md:table-cell text-right">
                      <Badge variant="outline" className="font-mono">
                        {trace.span_count}
                      </Badge>
                    </TableCell>
                    <TableCell className="hidden md:table-cell text-right text-sm text-muted-foreground">
                      {format(
                        new Date(trace.start_time),
                        'MMM d, HH:mm:ss.SSS'
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>

          {/* Pagination */}
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
            <div className="text-sm text-muted-foreground text-center sm:text-left">
              <span className="hidden sm:inline">
                Showing {(page - 1) * PAGE_SIZE + 1}–{Math.min(page * PAGE_SIZE, totalCount)} of{' '}
                {totalCount.toLocaleString()} trace{totalCount !== 1 ? 's' : ''}
              </span>
              <span className="sm:hidden">
                {totalCount.toLocaleString()} trace{totalCount !== 1 ? 's' : ''}
              </span>
            </div>
            <div className="flex items-center justify-center gap-1">
              <Button
                variant="outline"
                size="sm"
                onClick={() => setPage((p) => Math.max(1, p - 1))}
                disabled={page === 1}
              >
                <ChevronLeft className="h-4 w-4" />
              </Button>
              <span className="px-3 text-sm text-muted-foreground">
                {page} / {totalPages}
              </span>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setPage((p) => p + 1)}
                disabled={page >= totalPages}
              >
                <ChevronRight className="h-4 w-4" />
              </Button>
            </div>
          </div>
        </>
      )}
    </div>
  )
}
