import { ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getMetricsOverTimeOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
} from '@/components/ui/hover-card'
import { Progress } from '@/components/ui/progress'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { format, subDays } from 'date-fns'
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  Clock,
  Code2,
  Eye,
  Info,
  Monitor,
  RefreshCw,
  Smartphone,
  TrendingUp,
  Zap,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import {
  CartesianGrid,
  Legend,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  TooltipProps,
  XAxis,
  YAxis,
} from 'recharts'

const METRIC_THRESHOLDS = {
  fcp: { good: 1800, poor: 3000, unit: 'ms', label: 'First Contentful Paint' },
  lcp: {
    good: 2500,
    poor: 4000,
    unit: 'ms',
    label: 'Largest Contentful Paint',
  },
  fid: { good: 100, poor: 300, unit: 'ms', label: 'First Input Delay' },
  cls: { good: 0.1, poor: 0.25, unit: '', label: 'Cumulative Layout Shift' },
  ttfb: { good: 800, poor: 1800, unit: 'ms', label: 'Time to First Byte' },
  inp: { good: 200, poor: 500, unit: 'ms', label: 'Interaction to Next Paint' },
} as const

const METRIC_WEIGHTS = {
  fcp: 0.15,
  lcp: 0.3,
  inp: 0.3,
  cls: 0.25,
} as const

function calculateMetricScore(
  value: number,
  metric: keyof typeof METRIC_THRESHOLDS
) {
  const thresholds = METRIC_THRESHOLDS[metric]
  if (value <= thresholds.good) return 1
  if (value >= thresholds.poor) return 0
  const range = thresholds.poor - thresholds.good
  const valueFromGood = value - thresholds.good
  return 1 - valueFromGood / range
}

function calculateOverallScore(metrics: any): number {
  if (!metrics) return 0
  let totalScore = 0
  let totalWeight = 0

  if (metrics.fcp_p75) {
    totalScore +=
      calculateMetricScore(metrics.fcp_p75, 'fcp') * METRIC_WEIGHTS.fcp
    totalWeight += METRIC_WEIGHTS.fcp
  }
  if (metrics.lcp_p75) {
    totalScore +=
      calculateMetricScore(metrics.lcp_p75, 'lcp') * METRIC_WEIGHTS.lcp
    totalWeight += METRIC_WEIGHTS.lcp
  }
  if (metrics.inp_p75) {
    totalScore +=
      calculateMetricScore(metrics.inp_p75, 'inp') * METRIC_WEIGHTS.inp
    totalWeight += METRIC_WEIGHTS.inp
  }
  if (metrics.cls_p75) {
    totalScore +=
      calculateMetricScore(metrics.cls_p75, 'cls') * METRIC_WEIGHTS.cls
    totalWeight += METRIC_WEIGHTS.cls
  }

  if (totalWeight === 0) return 0
  return Math.round((totalScore / totalWeight) * 100)
}

// Custom tooltip component
const CustomTooltip = ({
  active,
  payload,
  label,
}: TooltipProps<number, string>) => {
  if (!active || !payload || !payload.length) return null

  return (
    <div className="rounded-lg border bg-background p-3 shadow-lg">
      <p className="mb-2 text-sm font-semibold">{label}</p>
      <div className="space-y-1">
        {payload.map((entry, index) => {
          const metric = entry.dataKey as string
          const value = entry.value as number

          // Get appropriate color and icon based on metric
          const getMetricStyle = () => {
            switch (metric) {
              case 'fcp':
                return { color: '#8884d8', icon: '⚡' }
              case 'lcp':
                return { color: '#82ca9d', icon: '📊' }
              case 'ttfb':
                return { color: '#ffc658', icon: '🔄' }
              case 'cls':
                return { color: '#ff6b6b', icon: '📐' }
              default:
                return { color: entry.color, icon: '•' }
            }
          }

          const style = getMetricStyle()
          const formattedValue =
            metric === 'cls'
              ? (value / 1000).toFixed(3)
              : `${value.toFixed(0)}ms`

          return (
            <div
              key={index}
              className="flex items-center justify-between gap-4 text-sm"
            >
              <div className="flex items-center gap-2">
                <span style={{ color: style.color }}>{style.icon}</span>
                <span className="text-muted-foreground">{entry.name}</span>
              </div>
              <span
                className="font-mono font-medium"
                style={{ color: style.color }}
              >
                {formattedValue}
              </span>
            </div>
          )
        })}
      </div>
    </div>
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

  // Fetch environments for the project
  const { data: environmentsData } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  // Set the first environment as default when environments are loaded
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
      },
    }),
    enabled: selectedEnvironment !== null, // Only fetch when environment is selected
    refetchInterval: 300000, // Refetch every 5 minutes
  })

  const chartData = useMemo(() => {
    if (!metrics?.timestamps) return []

    // Process data to handle nulls properly
    return metrics.timestamps.map((timestamp: string, i: number) => {
      const data: any = {
        timestamp: format(
          new Date(timestamp),
          timeRange === '1d' ? 'HH:mm' : 'MMM dd'
        ),
        rawTimestamp: timestamp,
      }

      // Only add metric values if they're not null
      if (metrics.fcp[i] !== null) data.fcp = metrics.fcp[i]
      if (metrics.lcp[i] !== null) data.lcp = metrics.lcp[i]
      if (metrics.ttfb[i] !== null) data.ttfb = metrics.ttfb[i]
      if (metrics.fid[i] !== null) data.fid = metrics.fid[i]
      if (metrics.cls[i] !== null) data.cls = metrics.cls[i] * 1000 // Scale CLS for visibility

      return data
    })
  }, [metrics, timeRange])

  const score = useMemo(() => {
    if (!metrics) return 0
    return calculateOverallScore(metrics)
  }, [metrics])

  // Check for data sparsity
  const dataSparseWarning = useMemo(() => {
    if (!metrics) return false

    // Count non-null values for each metric
    const countValid = (arr: any[]) =>
      arr?.filter((v) => v !== null).length || 0

    const totalPoints = metrics.timestamps?.length || 0
    const validFcp = countValid(metrics.fcp)
    const validLcp = countValid(metrics.lcp)
    const validTtfb = countValid(metrics.ttfb)

    // If less than 50% of data points are valid, show warning
    const avgValidPercent =
      ((validFcp + validLcp + validTtfb) / 3 / totalPoints) * 100

    return avgValidPercent < 50
  }, [metrics])

  // Check if we have no performance data at all
  const hasNoData = useMemo(() => {
    if (!metrics || isLoading) return false

    // Check if all metric arrays are empty or all values are null
    const countValid = (arr: any[]) =>
      arr?.filter((v) => v !== null && v !== undefined).length || 0

    const validFcp = countValid(metrics.fcp)
    const validLcp = countValid(metrics.lcp)
    const validTtfb = countValid(metrics.ttfb)
    const validFid = countValid(metrics.fid)
    const validCls = countValid(metrics.cls)

    return (
      validFcp === 0 &&
      validLcp === 0 &&
      validTtfb === 0 &&
      validFid === 0 &&
      validCls === 0
    )
  }, [metrics, isLoading])

  const getScoreColor = (score: number) => {
    if (score >= 90) return 'text-green-600'
    if (score >= 50) return 'text-orange-500'
    return 'text-red-500'
  }

  const getScoreStatus = (score: number) => {
    if (score >= 90)
      return { label: 'Good', icon: CheckCircle2, color: 'text-green-600' }
    if (score >= 50)
      return {
        label: 'Needs Improvement',
        icon: AlertTriangle,
        color: 'text-orange-500',
      }
    return { label: 'Poor', icon: AlertTriangle, color: 'text-red-500' }
  }

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

  // Show setup instructions if no data
  if (hasNoData && !isLoading) {
    return (
      <div className="space-y-6">
        {/* Header */}
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

        {/* No Data Alert */}
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

        {/* Setup Instructions - Redirect to Analytics */}
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
                      captures:
                    </p>
                    <ul className="mt-2 space-y-1 text-sm text-muted-foreground ml-4">
                      <li>• First Contentful Paint (FCP)</li>
                      <li>• Largest Contentful Paint (LCP)</li>
                      <li>• First Input Delay (FID)</li>
                      <li>• Interaction to Next Paint (INP)</li>
                      <li>• Cumulative Layout Shift (CLS)</li>
                      <li>• Time to First Byte (TTFB)</li>
                    </ul>
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
                <Button
                  onClick={() =>
                    (window.location.href = `/projects/${project.slug}/analytics/setup`)
                  }
                  className="w-full sm:w-auto"
                >
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

        {/* Additional Information */}
        <Card>
          <CardHeader>
            <CardTitle>What are Core Web Vitals?</CardTitle>
            <CardDescription>
              Key metrics that measure real-world user experience
            </CardDescription>
          </CardHeader>
          <CardContent>
            <div className="space-y-4">
              <div className="grid gap-4 sm:grid-cols-2">
                <div className="rounded-lg border p-4">
                  <div className="flex items-center gap-2 mb-2">
                    <Eye className="h-4 w-4 text-muted-foreground" />
                    <h4 className="font-medium text-sm">LCP - Loading</h4>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    Measures how quickly the main content loads. Target: &lt;
                    2.5s
                  </p>
                </div>
                <div className="rounded-lg border p-4">
                  <div className="flex items-center gap-2 mb-2">
                    <Zap className="h-4 w-4 text-muted-foreground" />
                    <h4 className="font-medium text-sm">INP - Interactivity</h4>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    Measures responsiveness to user interactions. Target: &lt;
                    200ms
                  </p>
                </div>
                <div className="rounded-lg border p-4">
                  <div className="flex items-center gap-2 mb-2">
                    <TrendingUp className="h-4 w-4 text-muted-foreground" />
                    <h4 className="font-medium text-sm">
                      CLS - Visual Stability
                    </h4>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    Measures unexpected layout shifts. Target: &lt; 0.1
                  </p>
                </div>
                <div className="rounded-lg border p-4">
                  <div className="flex items-center gap-2 mb-2">
                    <Clock className="h-4 w-4 text-muted-foreground" />
                    <h4 className="font-medium text-sm">
                      TTFB - Server Response
                    </h4>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    Measures server response time. Target: &lt; 800ms
                  </p>
                </div>
              </div>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">
            Performance Insights
          </h2>
          <p className="text-muted-foreground">
            Real user metrics and Core Web Vitals for your application
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => refetch()}
            disabled={isLoading}
          >
            <RefreshCw className={cn('h-4 w-4', isLoading && 'animate-spin')} />
            Refresh
          </Button>
        </div>
      </div>

      {/* Controls */}
      <Card>
        <CardContent className="pt-6">
          <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex flex-col gap-4 sm:flex-row sm:items-center">
              <div className="flex items-center gap-2">
                <Select
                  value={selectedEnvironment?.toString()}
                  onValueChange={(value) =>
                    setSelectedEnvironment(Number(value))
                  }
                  disabled={!environmentsData || environmentsData.length === 0}
                >
                  <SelectTrigger className="w-[140px]">
                    <SelectValue placeholder="Select environment" />
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
                  <TabsList>
                    <TabsTrigger
                      value="desktop"
                      className="flex items-center gap-2"
                    >
                      <Monitor className="h-4 w-4" />
                      Desktop
                    </TabsTrigger>
                    <TabsTrigger
                      value="mobile"
                      className="flex items-center gap-2"
                    >
                      <Smartphone className="h-4 w-4" />
                      Mobile
                    </TabsTrigger>
                  </TabsList>
                </Tabs>
              </div>
            </div>

            <Select value={timeRange} onValueChange={setTimeRange}>
              <SelectTrigger className="w-[120px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="1d">Last 24h</SelectItem>
                <SelectItem value="7d">Last 7 days</SelectItem>
                <SelectItem value="30d">Last 30 days</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </CardContent>
      </Card>

      {isLoading ? (
        <div className="grid gap-6 lg:grid-cols-[350px_1fr]">
          <div className="space-y-4">
            <Skeleton className="h-[400px] w-full" />
          </div>
          <div className="space-y-4">
            <Skeleton className="h-[400px] w-full" />
          </div>
        </div>
      ) : (
        <div className="grid gap-6 lg:grid-cols-[350px_1fr]">
          {/* Metrics Overview */}
          <div className="space-y-4">
            <Card>
              <CardHeader className="pb-4">
                <div className="flex items-center justify-between">
                  <div>
                    <CardTitle className="text-lg">Performance Score</CardTitle>
                    <CardDescription>Based on Core Web Vitals</CardDescription>
                  </div>
                  <HoverCard>
                    <HoverCardTrigger asChild>
                      <Button variant="ghost" size="icon" className="h-8 w-8">
                        <Info className="h-4 w-4" />
                      </Button>
                    </HoverCardTrigger>
                    <HoverCardContent className="w-80">
                      <div className="space-y-2">
                        <h4 className="text-sm font-semibold">
                          How is this calculated?
                        </h4>
                        <p className="text-sm text-muted-foreground">
                          The performance score is a weighted average of your
                          Core Web Vitals:
                        </p>
                        <ul className="text-sm text-muted-foreground space-y-1">
                          <li>• First Contentful Paint (15%)</li>
                          <li>• Largest Contentful Paint (30%)</li>
                          <li>• Interaction to Next Paint (30%)</li>
                          <li>• Cumulative Layout Shift (25%)</li>
                        </ul>
                        <div className="pt-2 border-t">
                          <div className="text-xs space-y-1">
                            <div className="flex items-center gap-2">
                              <div className="w-3 h-3 rounded-full bg-green-500" />
                              <span>90-100: Good</span>
                            </div>
                            <div className="flex items-center gap-2">
                              <div className="w-3 h-3 rounded-full bg-orange-500" />
                              <span>50-89: Needs Improvement</span>
                            </div>
                            <div className="flex items-center gap-2">
                              <div className="w-3 h-3 rounded-full bg-red-500" />
                              <span>0-49: Poor</span>
                            </div>
                          </div>
                        </div>
                      </div>
                    </HoverCardContent>
                  </HoverCard>
                </div>
              </CardHeader>
              <CardContent>
                <div className="flex items-center gap-6">
                  <div className="relative">
                    <div className="flex h-20 w-20 items-center justify-center rounded-full border-8 border-muted">
                      <span
                        className={cn(
                          'text-2xl font-bold',
                          getScoreColor(score)
                        )}
                      >
                        {score}
                      </span>
                    </div>
                    <div
                      className={cn(
                        'absolute inset-0 rounded-full border-8 border-transparent',
                        {
                          'border-t-green-600': score >= 90,
                          'border-t-orange-500': score >= 50 && score < 90,
                          'border-t-red-500': score < 50,
                        }
                      )}
                      style={{
                        transform: `rotate(${(score / 100) * 360 - 90}deg)`,
                      }}
                    />
                  </div>
                  <div className="space-y-2">
                    {(() => {
                      const status = getScoreStatus(score)
                      return (
                        <div className="flex items-center gap-2">
                          <status.icon
                            className={cn('h-4 w-4', status.color)}
                          />
                          <span className={cn('font-medium', status.color)}>
                            {status.label}
                          </span>
                        </div>
                      )
                    })()}
                    <p className="text-sm text-muted-foreground">
                      {score >= 90
                        ? 'Excellent performance'
                        : score >= 50
                          ? 'Room for improvement'
                          : 'Significant issues detected'}
                    </p>
                  </div>
                </div>
              </CardContent>
            </Card>

            {metrics && (
              <Card>
                <CardHeader>
                  <CardTitle className="text-lg">Core Web Vitals</CardTitle>
                  <CardDescription>75th percentile values</CardDescription>
                </CardHeader>
                <CardContent className="space-y-4">
                  <MetricCard
                    label="First Contentful Paint"
                    value={metrics.fcp_p75 || 0}
                    unit="ms"
                    threshold={METRIC_THRESHOLDS.fcp}
                    icon={<Eye className="h-4 w-4" />}
                  />
                  <MetricCard
                    label="Largest Contentful Paint"
                    value={metrics.lcp_p75 || 0}
                    unit="ms"
                    threshold={METRIC_THRESHOLDS.lcp}
                    icon={<Activity className="h-4 w-4" />}
                  />
                  <MetricCard
                    label="First Input Delay"
                    value={metrics.fid_p75 || 0}
                    unit="ms"
                    threshold={METRIC_THRESHOLDS.fid}
                    icon={<Zap className="h-4 w-4" />}
                  />
                  <MetricCard
                    label="Cumulative Layout Shift"
                    value={metrics.cls_p75 || 0}
                    unit=""
                    threshold={METRIC_THRESHOLDS.cls}
                    icon={<TrendingUp className="h-4 w-4" />}
                  />
                  <MetricCard
                    label="Time to First Byte"
                    value={metrics.ttfb_p75 || 0}
                    unit="ms"
                    threshold={METRIC_THRESHOLDS.ttfb}
                    icon={<Clock className="h-4 w-4" />}
                  />
                </CardContent>
              </Card>
            )}
          </div>

          {/* Charts */}
          <div className="space-y-4">
            {dataSparseWarning && (
              <Alert>
                <AlertTriangle className="h-4 w-4" />
                <AlertDescription>
                  Limited performance data available. Metrics are being
                  collected as users visit your site. Data points with missing
                  values indicate periods with no user activity.
                </AlertDescription>
              </Alert>
            )}

            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <TrendingUp className="h-5 w-5" />
                  Performance Trends
                </CardTitle>
                <CardDescription>
                  Metrics over time (75th percentile)
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="h-[350px]">
                  <ResponsiveContainer width="100%" height="100%">
                    <LineChart
                      data={chartData}
                      margin={{ top: 5, right: 30, left: 20, bottom: 5 }}
                    >
                      <defs>
                        <linearGradient
                          id="colorFcp"
                          x1="0"
                          y1="0"
                          x2="0"
                          y2="1"
                        >
                          <stop
                            offset="5%"
                            stopColor="#8884d8"
                            stopOpacity={0.3}
                          />
                          <stop
                            offset="95%"
                            stopColor="#8884d8"
                            stopOpacity={0}
                          />
                        </linearGradient>
                        <linearGradient
                          id="colorLcp"
                          x1="0"
                          y1="0"
                          x2="0"
                          y2="1"
                        >
                          <stop
                            offset="5%"
                            stopColor="#82ca9d"
                            stopOpacity={0.3}
                          />
                          <stop
                            offset="95%"
                            stopColor="#82ca9d"
                            stopOpacity={0}
                          />
                        </linearGradient>
                        <linearGradient
                          id="colorTtfb"
                          x1="0"
                          y1="0"
                          x2="0"
                          y2="1"
                        >
                          <stop
                            offset="5%"
                            stopColor="#ffc658"
                            stopOpacity={0.3}
                          />
                          <stop
                            offset="95%"
                            stopColor="#ffc658"
                            stopOpacity={0}
                          />
                        </linearGradient>
                      </defs>
                      <CartesianGrid
                        strokeDasharray="3 3"
                        stroke="hsl(var(--border))"
                        opacity={0.3}
                      />
                      <XAxis
                        dataKey="timestamp"
                        stroke="hsl(var(--muted-foreground))"
                        fontSize={11}
                        tick={{ fill: 'hsl(var(--muted-foreground))' }}
                      />
                      <YAxis
                        stroke="hsl(var(--muted-foreground))"
                        fontSize={11}
                        tick={{ fill: 'hsl(var(--muted-foreground))' }}
                        label={{
                          value: 'Time (ms)',
                          angle: -90,
                          position: 'insideLeft',
                          style: {
                            fontSize: 11,
                            fill: 'hsl(var(--muted-foreground))',
                          },
                        }}
                      />
                      <Tooltip content={<CustomTooltip />} />
                      <Legend
                        wrapperStyle={{
                          paddingTop: '20px',
                        }}
                        iconType="line"
                        formatter={(value) => (
                          <span className="text-xs text-muted-foreground">
                            {value}
                          </span>
                        )}
                        iconSize={18}
                      />
                      <Line
                        type="monotone"
                        dataKey="fcp"
                        stroke="#8884d8"
                        strokeWidth={2.5}
                        name="First Contentful Paint"
                        connectNulls={true}
                        dot={false}
                        activeDot={{ r: 6, strokeWidth: 0 }}
                        strokeLinecap="round"
                      />
                      <Line
                        type="monotone"
                        dataKey="lcp"
                        stroke="#82ca9d"
                        strokeWidth={2.5}
                        name="Largest Contentful Paint"
                        connectNulls={true}
                        dot={false}
                        activeDot={{ r: 6, strokeWidth: 0 }}
                        strokeLinecap="round"
                      />
                      <Line
                        type="monotone"
                        dataKey="ttfb"
                        stroke="#ffc658"
                        strokeWidth={2.5}
                        name="Time to First Byte"
                        connectNulls={true}
                        dot={false}
                        activeDot={{ r: 6, strokeWidth: 0 }}
                        strokeLinecap="round"
                      />
                    </LineChart>
                  </ResponsiveContainer>
                </div>
              </CardContent>
            </Card>

            {/* Recommendations */}
            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <Info className="h-5 w-5" />
                  Performance Recommendations
                </CardTitle>
              </CardHeader>
              <CardContent>
                <div className="space-y-3">
                  {score < 90 && (
                    <div className="flex items-start gap-3 rounded-lg border p-3">
                      <AlertTriangle className="h-5 w-5 text-orange-500 mt-0.5" />
                      <div>
                        <h4 className="font-medium">
                          Optimize Core Web Vitals
                        </h4>
                        <p className="text-sm text-muted-foreground">
                          Focus on improving LCP, FID, and CLS for better user
                          experience.
                        </p>
                      </div>
                    </div>
                  )}
                  {metrics?.lcp_p75 && metrics?.lcp_p75 > 2500 && (
                    <div className="flex items-start gap-3 rounded-lg border p-3">
                      <Clock className="h-5 w-5 text-blue-500 mt-0.5" />
                      <div>
                        <h4 className="font-medium">
                          Reduce Largest Contentful Paint
                        </h4>
                        <p className="text-sm text-muted-foreground">
                          Optimize images, enable compression, and use a CDN.
                        </p>
                      </div>
                    </div>
                  )}
                  {metrics?.ttfb_p75 && metrics?.ttfb_p75 > 800 && (
                    <div className="flex items-start gap-3 rounded-lg border p-3">
                      <Zap className="h-5 w-5 text-purple-500 mt-0.5" />
                      <div>
                        <h4 className="font-medium">
                          Improve Server Response Time
                        </h4>
                        <p className="text-sm text-muted-foreground">
                          Optimize database queries and enable server-side
                          caching.
                        </p>
                      </div>
                    </div>
                  )}
                  {score >= 90 && (
                    <div className="flex items-start gap-3 rounded-lg border border-green-200 bg-green-50 dark:border-green-800 dark:bg-green-950 p-3">
                      <CheckCircle2 className="h-5 w-5 text-green-600 mt-0.5" />
                      <div>
                        <h4 className="font-medium text-green-800 dark:text-green-200">
                          Excellent Performance
                        </h4>
                        <p className="text-sm text-green-600 dark:text-green-400">
                          Your application meets all Core Web Vitals thresholds.
                        </p>
                      </div>
                    </div>
                  )}
                </div>
              </CardContent>
            </Card>
          </div>
        </div>
      )}
    </div>
  )
}

interface MetricCardProps {
  label: string
  value: number | null
  unit: string
  threshold: { good: number; poor: number }
  icon: React.ReactNode
}

function MetricCard({ label, value, unit, threshold, icon }: MetricCardProps) {
  if (!value) return null

  const getStatus = (val: number) => {
    if (val <= threshold.good)
      return {
        label: 'Good',
        color: 'text-green-600 bg-green-100 dark:bg-green-950',
      }
    if (val >= threshold.poor)
      return { label: 'Poor', color: 'text-red-600 bg-red-100 dark:bg-red-950' }
    return {
      label: 'Fair',
      color: 'text-orange-600 bg-orange-100 dark:bg-orange-950',
    }
  }

  const status = getStatus(value)
  const progress = Math.min((value / threshold.poor) * 100, 100)

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          {icon}
          <span className="text-sm font-medium">{label}</span>
        </div>
        <div className="text-right">
          <div className="text-sm font-mono">
            {value.toFixed(unit === '' ? 3 : 0)}
            {unit}
          </div>
          <Badge className={cn('text-xs', status.color)} variant="secondary">
            {status.label}
          </Badge>
        </div>
      </div>
      <div className="space-y-1">
        <Progress value={progress} className="h-2" />
        <div className="flex justify-between text-xs text-muted-foreground">
          <span>
            Target: {threshold.good}
            {unit}
          </span>
          <span>
            Poor: {threshold.poor}
            {unit}
          </span>
        </div>
      </div>
    </div>
  )
}
