import { ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getGroupedPageMetricsOptions,
  getMetricsOverTimeOptions,
  hasPerformanceMetricsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  MetricSparkline,
  type MetricTone,
} from '@/components/charts/metric-sparkline'
import { ScoreRing } from '@/components/charts/score-ring'
import {
  ThresholdLineChart,
  type ThresholdBand,
} from '@/components/charts/threshold-line-chart'
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
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { SpeedWorldMap } from '@/components/project/SpeedWorldMap'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { format, subDays } from 'date-fns'
import {
  AlertTriangle,
  CheckCircle2,
  Code2,
  Info,
  Monitor,
  RefreshCw,
  Smartphone,
  X,
  Zap,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'

type MetricKey = 'lcp' | 'inp' | 'cls' | 'ttfb' | 'fcp'

const METRIC_THRESHOLDS: Record<
  MetricKey,
  { good: number; poor: number; unit: 'ms' | ''; label: string; short: string }
> = {
  fcp: {
    good: 1800,
    poor: 3000,
    unit: 'ms',
    label: 'First Contentful Paint',
    short: 'FCP',
  },
  lcp: {
    good: 2500,
    poor: 4000,
    unit: 'ms',
    label: 'Largest Contentful Paint',
    short: 'LCP',
  },
  cls: {
    good: 0.1,
    poor: 0.25,
    unit: '',
    label: 'Cumulative Layout Shift',
    short: 'CLS',
  },
  ttfb: {
    good: 800,
    poor: 1800,
    unit: 'ms',
    label: 'Time to First Byte',
    short: 'TTFB',
  },
  inp: {
    good: 200,
    poor: 500,
    unit: 'ms',
    label: 'Interaction to Next Paint',
    short: 'INP',
  },
}

const METRIC_WEIGHTS = {
  fcp: 0.15,
  lcp: 0.3,
  inp: 0.3,
  cls: 0.25,
} as const

type MetricStatus = 'good' | 'needs-improvement' | 'poor'

function getMetricStatus(value: number, metric: MetricKey): MetricStatus {
  const t = METRIC_THRESHOLDS[metric]
  if (value <= t.good) return 'good'
  if (value >= t.poor) return 'poor'
  return 'needs-improvement'
}

function statusToTone(status: MetricStatus): MetricTone {
  if (status === 'good') return 'good'
  if (status === 'poor') return 'poor'
  return 'warn'
}

function formatMetricValue(
  value: number | null | undefined,
  metric: MetricKey
) {
  if (value === null || value === undefined) return '—'
  const t = METRIC_THRESHOLDS[metric]
  if (metric === 'cls') return value.toFixed(2)
  if (value >= 1000) return `${(value / 1000).toFixed(2)}s`
  return `${Math.round(value)}${t.unit}`
}

function calculateMetricScore(value: number, metric: MetricKey) {
  const t = METRIC_THRESHOLDS[metric]
  if (value <= t.good) return 1
  if (value >= t.poor) return 0
  return 1 - (value - t.good) / (t.poor - t.good)
}

function calculateOverallScore(metrics: any): number {
  if (!metrics) return 0
  let totalScore = 0
  let totalWeight = 0

  const add = (
    val: number | null | undefined,
    key: MetricKey,
    weight: number
  ) => {
    if (val !== null && val !== undefined && val > 0) {
      totalScore += calculateMetricScore(val, key) * weight
      totalWeight += weight
    }
  }

  add(metrics.fcp_p75, 'fcp', METRIC_WEIGHTS.fcp)
  add(metrics.lcp_p75, 'lcp', METRIC_WEIGHTS.lcp)
  add(metrics.inp_p75, 'inp', METRIC_WEIGHTS.inp)
  add(metrics.cls_p75, 'cls', METRIC_WEIGHTS.cls)

  if (totalWeight === 0) return 0
  return Math.round((totalScore / totalWeight) * 100)
}

function scoreTone(score: number): MetricTone {
  if (score >= 90) return 'good'
  if (score >= 50) return 'warn'
  return 'poor'
}

const STATUS_LABEL: Record<MetricStatus, string> = {
  good: 'Good',
  'needs-improvement': 'Needs work',
  poor: 'Poor',
}

const STATUS_CHIP: Record<MetricStatus, string> = {
  good: 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400 border-emerald-500/20',
  'needs-improvement':
    'bg-amber-500/10 text-amber-600 dark:text-amber-400 border-amber-500/20',
  poor: 'bg-red-500/10 text-red-600 dark:text-red-400 border-red-500/20',
}

const STATUS_TEXT: Record<MetricStatus, string> = {
  good: 'text-emerald-600 dark:text-emerald-400',
  'needs-improvement': 'text-amber-600 dark:text-amber-400',
  poor: 'text-red-600 dark:text-red-400',
}

interface MetricTileProps {
  metric: MetricKey
  value: number | null | undefined
  history: (number | null)[]
}

function MetricTile({ metric, value, history }: MetricTileProps) {
  const t = METRIC_THRESHOLDS[metric]
  const hasValue = value !== null && value !== undefined && value > 0
  const status: MetricStatus = hasValue
    ? getMetricStatus(value, metric)
    : 'needs-improvement'
  const tone = statusToTone(status)

  return (
    <div className="flex flex-col gap-2 rounded-lg border bg-card p-4 transition-colors hover:border-foreground/20">
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          {t.short}
        </span>
        {hasValue ? (
          <span
            className={cn(
              'rounded-full border px-2 py-0.5 text-[10px] font-medium',
              STATUS_CHIP[status]
            )}
          >
            {STATUS_LABEL[status]}
          </span>
        ) : (
          <span className="rounded-full border border-border px-2 py-0.5 text-[10px] font-medium text-muted-foreground">
            No data
          </span>
        )}
      </div>
      <div className="flex items-baseline gap-1">
        <span className="text-2xl font-semibold tabular-nums">
          {formatMetricValue(value, metric)}
        </span>
        <span className="text-[10px] text-muted-foreground">p75</span>
      </div>
      <MetricSparkline data={history} tone={hasValue ? tone : 'neutral'} />
    </div>
  )
}

type BreakdownDimension =
  | 'path'
  | 'country'
  | 'region'
  | 'city'
  | 'device_type'
  | 'browser'
  | 'operating_system'

const BREAKDOWN_DIMENSIONS: {
  value: BreakdownDimension
  label: string
  column: string
}[] = [
  { value: 'path', label: 'Pages', column: 'Page' },
  { value: 'country', label: 'Countries', column: 'Country' },
  { value: 'region', label: 'Regions', column: 'Region' },
  { value: 'city', label: 'Cities', column: 'City' },
  { value: 'device_type', label: 'Devices', column: 'Device' },
  { value: 'browser', label: 'Browsers', column: 'Browser' },
  { value: 'operating_system', label: 'OS', column: 'OS' },
]

const BREAKDOWN_ROW_LIMIT = 15

const BREAKDOWN_METRICS: MetricKey[] = ['ttfb', 'fcp', 'lcp', 'inp', 'cls']

/** Segment filters accepted by all performance read endpoints. */
type SpeedFilters = {
  filter_path?: string
  filter_country?: string
  filter_region?: string
  filter_city?: string
  filter_browser?: string
  filter_operating_system?: string
}

const FILTER_LABELS: Record<keyof SpeedFilters, string> = {
  filter_path: 'Page',
  filter_country: 'Country',
  filter_region: 'Region',
  filter_city: 'City',
  filter_browser: 'Browser',
  filter_operating_system: 'OS',
}

// Which filter a breakdown row click applies. Device rows are not clickable —
// the Desktop/Mobile toggle already covers that dimension.
const DIMENSION_FILTER_KEY: Partial<
  Record<BreakdownDimension, keyof SpeedFilters>
> = {
  path: 'filter_path',
  country: 'filter_country',
  region: 'filter_region',
  city: 'filter_city',
  browser: 'filter_browser',
  operating_system: 'filter_operating_system',
}

function BreakdownMetricCell({
  value,
  metric,
}: {
  value: number | null | undefined
  metric: MetricKey
}) {
  const status =
    value !== null && value !== undefined
      ? getMetricStatus(value, metric)
      : null
  return (
    <TableCell
      className={cn(
        'text-right tabular-nums',
        status ? STATUS_TEXT[status] : 'text-muted-foreground'
      )}
    >
      {formatMetricValue(value, metric)}
    </TableCell>
  )
}

interface SpeedBreakdownCardProps {
  projectId: number
  environmentId: number | null
  startDate: string
  endDate: string
  device: 'desktop' | 'mobile'
  includeBots: boolean
  filters: SpeedFilters
  onFilter: (key: keyof SpeedFilters, value: string) => void
}

function SpeedBreakdownCard({
  projectId,
  environmentId,
  startDate,
  endDate,
  device,
  includeBots,
  filters,
  onFilter,
}: SpeedBreakdownCardProps) {
  const [dimension, setDimension] = useState<BreakdownDimension>('path')

  const { data, isLoading, isError } = useQuery({
    ...getGroupedPageMetricsOptions({
      query: {
        start_date: startDate,
        end_date: endDate,
        project_id: projectId,
        environment_id: environmentId!,
        device_type: device,
        include_bots: includeBots,
        group_by: dimension,
        ...filters,
      },
    }),
    enabled: environmentId !== null,
  })

  const groups = data?.groups ?? []
  const visibleGroups = groups.slice(0, BREAKDOWN_ROW_LIMIT)
  const dimensionMeta = BREAKDOWN_DIMENSIONS.find((d) => d.value === dimension)
  const dimensionLabel = dimensionMeta?.label ?? ''

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <CardTitle className="text-base">Breakdown</CardTitle>
            <CardDescription>
              Average per group · sorted by samples · click a value to filter
            </CardDescription>
          </div>
          <Tabs
            value={dimension}
            onValueChange={(v) => setDimension(v as BreakdownDimension)}
          >
            <TabsList className="h-8">
              {BREAKDOWN_DIMENSIONS.map((d) => (
                <TabsTrigger
                  key={d.value}
                  value={d.value}
                  className="h-6 px-2.5 text-xs"
                >
                  {d.label}
                </TabsTrigger>
              ))}
            </TabsList>
          </Tabs>
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-2">
            {Array.from({ length: 5 }).map((_, i) => (
              <Skeleton key={i} className="h-8 w-full" />
            ))}
          </div>
        ) : isError ? (
          <Alert>
            <AlertTriangle className="h-4 w-4" />
            <AlertDescription>
              Failed to load the {dimensionLabel.toLowerCase()} breakdown.
            </AlertDescription>
          </Alert>
        ) : groups.length === 0 ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            No samples recorded for this period.
          </p>
        ) : (
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{dimensionMeta?.column ?? ''}</TableHead>
                  <TableHead className="text-right">Samples</TableHead>
                  {BREAKDOWN_METRICS.map((m) => (
                    <TableHead key={m} className="text-right">
                      {METRIC_THRESHOLDS[m].short}
                    </TableHead>
                  ))}
                </TableRow>
              </TableHeader>
              <TableBody>
                {visibleGroups.map((g) => {
                  const filterKey = DIMENSION_FILTER_KEY[dimension]
                  const filterable = filterKey && g.group_key !== 'Unknown'
                  return (
                    <TableRow key={g.group_key}>
                      <TableCell
                        className="max-w-[280px] truncate font-medium"
                        title={g.group_key}
                      >
                        {filterable ? (
                          <button
                            type="button"
                            className="cursor-pointer truncate hover:underline"
                            onClick={() => onFilter(filterKey, g.group_key)}
                          >
                            {g.group_key}
                          </button>
                        ) : (
                          g.group_key
                        )}
                      </TableCell>
                      <TableCell className="text-right tabular-nums text-muted-foreground">
                        {g.events.toLocaleString()}
                      </TableCell>
                      {BREAKDOWN_METRICS.map((m) => (
                        <BreakdownMetricCell key={m} value={g[m]} metric={m} />
                      ))}
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
            {groups.length > BREAKDOWN_ROW_LIMIT && (
              <p className="pt-2 text-xs text-muted-foreground">
                Top {BREAKDOWN_ROW_LIMIT} of {groups.length} groups ·{' '}
                {(data?.total_events ?? 0).toLocaleString()} samples total
              </p>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

interface ProjectSpeedInsightsProps {
  project: ProjectResponse
}

export function ProjectSpeedInsights({ project }: ProjectSpeedInsightsProps) {
  const [selectedEnvironment, setSelectedEnvironment] = useState<number | null>(
    null
  )
  const [device, setDevice] = useState<'desktop' | 'mobile'>('desktop')
  const [timeRange, setTimeRange] = useState('7d')
  const [activeMetric, setActiveMetric] = useState<MetricKey>('lcp')
  const [filters, setFilters] = useState<SpeedFilters>({})
  // Bots (crawler UAs + datacenter IPs) are always stored at ingest but
  // hidden from this page by default — they report unrepresentative timings.
  const [includeBots, setIncludeBots] = useState(false)

  const addFilter = (key: keyof SpeedFilters, value: string) =>
    setFilters((prev) => ({ ...prev, [key]: value }))
  const removeFilter = (key: keyof SpeedFilters) =>
    setFilters((prev) => {
      const next = { ...prev }
      delete next[key]
      return next
    })
  const activeFilters = Object.entries(filters) as [
    keyof SpeedFilters,
    string,
  ][]

  const { data: environmentsData } = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
  })

  useEffect(() => {
    if (
      environmentsData &&
      environmentsData.length > 0 &&
      selectedEnvironment === null
    ) {
      setSelectedEnvironment(environmentsData[0].id)
    }
  }, [environmentsData, selectedEnvironment])

  const getDays = (range: string) => {
    switch (range) {
      case '1d':
        return 1
      case '7d':
        return 7
      case '30d':
        return 30
      default:
        return 7
    }
  }

  const startDate = useMemo(
    () => subDays(new Date(), getDays(timeRange)).toISOString(),
    [timeRange]
  )
  const endDate = useMemo(() => new Date().toISOString(), [])

  const { data: hasMetricsData } = useQuery({
    ...hasPerformanceMetricsOptions({
      query: { project_id: project.id },
    }),
  })

  const {
    data: metrics,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...getMetricsOverTimeOptions({
      query: {
        start_date: startDate,
        end_date: endDate,
        project_id: project.id,
        environment_id: selectedEnvironment!,
        device_type: device,
        include_bots: includeBots,
        ...filters,
      },
    }),
    enabled: selectedEnvironment !== null,
    refetchInterval: 300000,
  })

  const chartData = useMemo(() => {
    if (!metrics?.timestamps) return []
    return metrics.timestamps.map((timestamp: string, i: number) => ({
      timestamp: format(
        new Date(timestamp),
        timeRange === '1d' ? 'HH:mm' : 'MMM dd'
      ),
      fcp: metrics.fcp[i],
      lcp: metrics.lcp[i],
      ttfb: metrics.ttfb[i],
      inp: metrics.inp?.[i] ?? null,
      // CLS is stored as a ratio; display in raw units (no scaling).
      cls: metrics.cls[i],
    }))
  }, [metrics, timeRange])

  const score = useMemo(
    () => (metrics ? calculateOverallScore(metrics) : 0),
    [metrics]
  )

  const hasNoDataAtAll = hasMetricsData?.has_metrics === false

  const hasNoFilteredData = useMemo(() => {
    if (!metrics || isLoading) return false
    const countValid = (arr: any[]) =>
      arr?.filter((v) => v !== null && v !== undefined).length || 0
    return (
      countValid(metrics.fcp) === 0 &&
      countValid(metrics.lcp) === 0 &&
      countValid(metrics.ttfb) === 0 &&
      countValid(metrics.fid) === 0 &&
      countValid(metrics.cls) === 0
    )
  }, [metrics, isLoading])

  const metricHistory: Record<MetricKey, (number | null)[]> = useMemo(
    () => ({
      fcp: metrics?.fcp ?? [],
      lcp: metrics?.lcp ?? [],
      inp: metrics?.inp ?? [],
      cls: metrics?.cls ?? [],
      ttfb: metrics?.ttfb ?? [],
    }),
    [metrics]
  )

  if (error) {
    return (
      <Alert>
        <AlertTriangle className="h-4 w-4" />
        <AlertDescription>
          Failed to load performance metrics. Please try again later.
        </AlertDescription>
      </Alert>
    )
  }

  if (hasNoDataAtAll && !isLoading) {
    return (
      <div className="space-y-6">
        <Card>
          <CardHeader>
            <div className="flex items-start gap-3">
              <div className="rounded-lg bg-primary/10 p-2">
                <Info className="h-5 w-5 text-primary" />
              </div>
              <div className="space-y-1">
                <CardTitle>Performance Metrics Setup Required</CardTitle>
                <CardDescription>
                  Performance metrics are automatically collected when you set
                  up analytics
                </CardDescription>
              </div>
            </div>
          </CardHeader>
        </Card>

        <Card className="border-yellow-200 bg-yellow-50 dark:border-yellow-800 dark:bg-yellow-950/50">
          <CardHeader>
            <div className="flex items-center gap-2">
              <Info className="h-4 w-4 text-yellow-600 dark:text-yellow-400" />
              <CardTitle className="text-base text-yellow-900 dark:text-yellow-100">
                No performance data detected
              </CardTitle>
            </div>
            <CardDescription className="text-yellow-700 dark:text-yellow-300">
              Performance metrics (Core Web Vitals) are collected automatically
              when you integrate the analytics SDK. Set up analytics to start
              tracking performance data.
            </CardDescription>
          </CardHeader>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Setup Analytics to Track Performance</CardTitle>
            <CardDescription>
              The Temps analytics SDK automatically tracks Core Web Vitals
              alongside page views and events
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            <div className="rounded-lg border border-muted bg-muted/30 p-6">
              <div className="space-y-4">
                <div className="flex items-start gap-3">
                  <CheckCircle2 className="h-5 w-5 text-green-600 mt-0.5 flex-shrink-0" />
                  <div>
                    <h4 className="font-medium mb-1">
                      Automatic Web Vitals Tracking
                    </h4>
                    <p className="text-sm text-muted-foreground">
                      When you install the Temps analytics SDK, it automatically
                      captures LCP, INP, CLS, FCP, TTFB, and FID.
                    </p>
                  </div>
                </div>
                <div className="flex items-start gap-3">
                  <Zap className="h-5 w-5 text-blue-600 mt-0.5 flex-shrink-0" />
                  <div>
                    <h4 className="font-medium mb-1">Real User Monitoring</h4>
                    <p className="text-sm text-muted-foreground">
                      Performance data is collected from real users, giving you
                      accurate insights into how your application performs in
                      production.
                    </p>
                  </div>
                </div>
              </div>
            </div>

            <div className="flex flex-col gap-3">
              <Link to={`/projects/${project.slug}/analytics/setup`}>
                <Button className="w-full sm:w-auto">
                  <Code2 className="mr-2 h-4 w-4" />
                  Go to Analytics Setup
                </Button>
              </Link>
              <p className="text-sm text-muted-foreground">
                Once analytics is configured, performance metrics will appear
                here automatically.
              </p>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  const tileMetrics: MetricKey[] = ['lcp', 'inp', 'cls', 'ttfb']
  const metricValueMap: Record<MetricKey, number | null | undefined> = {
    lcp: metrics?.lcp_p75,
    inp: metrics?.inp_p75,
    cls: metrics?.cls_p75,
    ttfb: metrics?.ttfb_p75,
    fcp: metrics?.fcp_p75,
  }

  const activeThreshold = METRIC_THRESHOLDS[activeMetric]
  const activeValue = metricValueMap[activeMetric]
  const activeTone: MetricTone =
    activeValue !== null && activeValue !== undefined && activeValue > 0
      ? statusToTone(getMetricStatus(activeValue, activeMetric))
      : 'neutral'

  const thresholdBands: ThresholdBand[] = [
    {
      value: activeThreshold.good,
      tone: 'good',
      label: `Good (${formatMetricValue(activeThreshold.good, activeMetric)})`,
    },
    {
      value: activeThreshold.poor,
      tone: 'poor',
      label: `Poor (${formatMetricValue(activeThreshold.poor, activeMetric)})`,
    },
  ]

  const failingMetrics = tileMetrics.filter((m) => {
    const v = metricValueMap[m]
    return (
      v !== null && v !== undefined && v > 0 && getMetricStatus(v, m) !== 'good'
    )
  })

  const overallStatus: MetricStatus =
    score >= 90 ? 'good' : score >= 50 ? 'needs-improvement' : 'poor'

  return (
    <div className="space-y-5">
      {/* Compact header with inline controls */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="space-y-0.5">
          <h2 className="text-xl font-semibold tracking-tight">
            Performance Insights
          </h2>
          <p className="text-sm text-muted-foreground">
            Real user Core Web Vitals
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Select
            value={selectedEnvironment?.toString()}
            onValueChange={(value) => setSelectedEnvironment(Number(value))}
            disabled={!environmentsData || environmentsData.length === 0}
          >
            <SelectTrigger className="h-8 w-[130px] text-xs">
              <SelectValue placeholder="Environment" />
            </SelectTrigger>
            <SelectContent>
              {environmentsData?.map((env) => (
                <SelectItem key={env.id} value={env.id.toString()}>
                  {env.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>

          <Tabs
            value={device}
            onValueChange={(v) => setDevice(v as 'desktop' | 'mobile')}
          >
            <TabsList className="h-8">
              <TabsTrigger value="desktop" className="h-6 gap-1.5 px-2 text-xs">
                <Monitor className="h-3.5 w-3.5" />
                Desktop
              </TabsTrigger>
              <TabsTrigger value="mobile" className="h-6 gap-1.5 px-2 text-xs">
                <Smartphone className="h-3.5 w-3.5" />
                Mobile
              </TabsTrigger>
            </TabsList>
          </Tabs>

          <Select value={timeRange} onValueChange={setTimeRange}>
            <SelectTrigger className="h-8 w-[110px] text-xs">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="1d">Last 24h</SelectItem>
              <SelectItem value="7d">Last 7 days</SelectItem>
              <SelectItem value="30d">Last 30 days</SelectItem>
            </SelectContent>
          </Select>

          <Select
            value={includeBots ? 'all' : 'human'}
            onValueChange={(v) => setIncludeBots(v === 'all')}
          >
            <SelectTrigger className="h-8 w-[130px] text-xs">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="human">Human traffic</SelectItem>
              <SelectItem value="all">All traffic</SelectItem>
            </SelectContent>
          </Select>

          <Button
            variant="outline"
            size="sm"
            className="h-8"
            onClick={() => refetch()}
            disabled={isLoading}
          >
            <RefreshCw
              className={cn('h-3.5 w-3.5', isLoading && 'animate-spin')}
            />
          </Button>
        </div>
      </div>

      {/* Active segment filters — applied to every card on this page */}
      {activeFilters.length > 0 && (
        <div className="flex flex-wrap items-center gap-2">
          {activeFilters.map(([key, value]) => (
            <button
              key={key}
              type="button"
              onClick={() => removeFilter(key)}
              className="inline-flex cursor-pointer items-center gap-1.5 rounded-full border bg-muted px-2.5 py-1 text-xs font-medium hover:bg-muted/60"
            >
              <span className="text-muted-foreground">
                {FILTER_LABELS[key]}:
              </span>
              <span className="max-w-[220px] truncate">{value}</span>
              <X className="h-3 w-3 text-muted-foreground" />
            </button>
          ))}
          <button
            type="button"
            onClick={() => setFilters({})}
            className="cursor-pointer text-xs text-muted-foreground hover:text-foreground hover:underline"
          >
            Clear all
          </button>
        </div>
      )}

      {isLoading ? (
        <div className="space-y-5">
          <Skeleton className="h-[120px] w-full" />
          <Skeleton className="h-[380px] w-full" />
        </div>
      ) : hasNoFilteredData ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-16">
            {device === 'mobile' ? (
              <Smartphone className="h-10 w-10 text-muted-foreground mb-4" />
            ) : (
              <Monitor className="h-10 w-10 text-muted-foreground mb-4" />
            )}
            <h3 className="text-lg font-semibold mb-1">
              No {device} data available
            </h3>
            <p className="text-sm text-muted-foreground text-center max-w-md">
              No performance metrics have been recorded for {device} devices in
              the selected time range. Try switching to{' '}
              {device === 'desktop' ? 'mobile' : 'desktop'} or selecting a
              different time range.
            </p>
          </CardContent>
        </Card>
      ) : (
        <>
          {/* Hero strip */}
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
            <div className="flex items-center gap-4 rounded-lg border bg-card p-4">
              <ScoreRing score={score} tone={scoreTone(score)} />
              <div className="min-w-0 space-y-1">
                <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                  Overall
                </div>
                <div
                  className={cn(
                    'text-sm font-medium',
                    STATUS_TEXT[overallStatus]
                  )}
                >
                  {STATUS_LABEL[overallStatus]}
                </div>
                <div className="text-xs text-muted-foreground">
                  Weighted Web Vitals
                </div>
              </div>
            </div>
            {tileMetrics.map((m) => (
              <MetricTile
                key={m}
                metric={m}
                value={metricValueMap[m]}
                history={metricHistory[m]}
              />
            ))}
          </div>

          {/* Tabbed trend chart */}
          <Card>
            <CardHeader className="pb-3">
              <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                <div>
                  <CardTitle className="text-base">Trend</CardTitle>
                  <CardDescription>
                    p75 over time · green/red lines show Good and Poor
                    thresholds
                  </CardDescription>
                </div>
                <Tabs
                  value={activeMetric}
                  onValueChange={(v) => setActiveMetric(v as MetricKey)}
                >
                  <TabsList className="h-8">
                    {tileMetrics.map((m) => (
                      <TabsTrigger
                        key={m}
                        value={m}
                        className="h-6 px-2.5 text-xs"
                      >
                        {METRIC_THRESHOLDS[m].short}
                      </TabsTrigger>
                    ))}
                  </TabsList>
                </Tabs>
              </div>
            </CardHeader>
            <CardContent>
              <ThresholdLineChart
                data={chartData}
                xKey="timestamp"
                series={{
                  dataKey: activeMetric,
                  tone: activeTone,
                  label: activeThreshold.label,
                }}
                thresholds={thresholdBands}
                height={300}
                yTickFormatter={(v) => {
                  if (activeMetric === 'cls') return v.toFixed(2)
                  if (v >= 1000) return `${(v / 1000).toFixed(1)}s`
                  return `${v}`
                }}
                tooltipValueFormatter={(v) =>
                  formatMetricValue(v, activeMetric)
                }
                tooltipFooter={(v) => {
                  const status = getMetricStatus(v, activeMetric)
                  return STATUS_LABEL[status]
                }}
              />
            </CardContent>
          </Card>

          {/* Web vitals by country, on a world map */}
          <SpeedWorldMap
            projectId={project.id}
            environmentId={selectedEnvironment}
            startDate={startDate}
            endDate={endDate}
            device={device}
            includeBots={includeBots}
            filters={filters}
            onCountryClick={(country) => addFilter('filter_country', country)}
          />

          {/* Per-page / per-dimension breakdown */}
          <SpeedBreakdownCard
            projectId={project.id}
            environmentId={selectedEnvironment}
            startDate={startDate}
            endDate={endDate}
            device={device}
            includeBots={includeBots}
            filters={filters}
            onFilter={addFilter}
          />

          {/* Targeted recommendations */}
          {failingMetrics.length > 0 ? (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-base">Recommendations</CardTitle>
                <CardDescription>
                  {failingMetrics.length} metric
                  {failingMetrics.length === 1 ? '' : 's'} below target
                </CardDescription>
              </CardHeader>
              <CardContent className="space-y-2">
                {failingMetrics.map((m) => {
                  const v = metricValueMap[m] as number
                  const status = getMetricStatus(v, m)
                  const t = METRIC_THRESHOLDS[m]
                  const targetLabel =
                    m === 'cls' ? t.good.toFixed(2) : `${t.good}${t.unit}`
                  return (
                    <div
                      key={m}
                      className="flex items-start gap-3 rounded-lg border p-3"
                    >
                      <div
                        className={cn(
                          'mt-0.5 rounded-md border p-1.5',
                          STATUS_CHIP[status]
                        )}
                      >
                        <AlertTriangle className="h-3.5 w-3.5" />
                      </div>
                      <div className="flex-1 space-y-0.5">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="text-sm font-medium">{t.label}</span>
                          <span className={cn('text-xs', STATUS_TEXT[status])}>
                            {formatMetricValue(v, m)} · target {targetLabel}
                          </span>
                        </div>
                        <p className="text-xs text-muted-foreground">
                          {recommendationFor(m)}
                        </p>
                      </div>
                    </div>
                  )
                })}
              </CardContent>
            </Card>
          ) : score > 0 ? (
            <Card className="border-emerald-500/20 bg-emerald-500/5">
              <CardContent className="flex items-center gap-3 py-4">
                <CheckCircle2 className="h-5 w-5 text-emerald-600 dark:text-emerald-400" />
                <div>
                  <div className="text-sm font-medium text-emerald-700 dark:text-emerald-300">
                    All Core Web Vitals pass
                  </div>
                  <div className="text-xs text-emerald-700/70 dark:text-emerald-400/80">
                    Your application meets every threshold. Keep monitoring for
                    regressions.
                  </div>
                </div>
              </CardContent>
            </Card>
          ) : null}
        </>
      )}
    </div>
  )
}

function recommendationFor(metric: MetricKey): string {
  switch (metric) {
    case 'lcp':
      return 'Optimize the largest image or text block: preload critical assets, compress images, and serve from a CDN close to users.'
    case 'inp':
      return 'Reduce long JavaScript tasks on interaction. Defer non-critical scripts, break up expensive handlers, and avoid layout thrashing.'
    case 'cls':
      return 'Reserve space for images, ads, and embeds. Avoid inserting content above existing content after the page has loaded.'
    case 'ttfb':
      return 'Improve server response: cache HTML/edge-render, optimize database queries, or move the origin closer to your users.'
    case 'fcp':
      return 'Ship less render-blocking CSS/JS and inline critical styles so the first paint happens sooner.'
  }
}
