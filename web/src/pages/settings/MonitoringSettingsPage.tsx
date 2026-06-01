import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import type { MonitoringSettings, MetricsStoreKind } from '@/api/platformSettings'
import {
  AlertCircle,
  BarChart2,
  Database,
  HardDrive,
  Loader2,
  Save,
} from 'lucide-react'
import { useEffect } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'

interface MonitoringFormData {
  monitoring: MonitoringSettings
}

const DEFAULTS: MonitoringSettings = {
  enabled: false,
  store: 'timescale_db',
  scrape_interval_secs: 30,
  retention_raw_days: 7,
  retention_hourly_days: 90,
  retention_daily_years: 2,
  clickhouse_url: null,
}

// Bytes per raw metric row (approximate: time 8 + source_kind 12 + source_id 4 +
// name 20 + value 8 + labels 32 = ~84 bytes; round up to 100 for overhead)
const BYTES_PER_ROW = 100
// Approximate number of metrics tracked per monitored service
const METRICS_PER_SERVICE = 15

function estimateStorageMbPerDay(scrapeIntervalSecs: number): number {
  // Placeholder: pull the "monitored services" count from settings if available
  // For the estimate we use a hard-coded representative value of 5 services.
  const monitoredServices = 5
  const scrapesPerDay = Math.floor((24 * 3600) / Math.max(scrapeIntervalSecs, 15))
  const rowsPerDay = monitoredServices * METRICS_PER_SERVICE * scrapesPerDay
  const bytesPerDay = rowsPerDay * BYTES_PER_ROW
  return Math.round(bytesPerDay / (1024 * 1024))
}

export function MonitoringSettingsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings, isLoading, error } = useSettings()
  const updateSettings = useUpdateSettings()

  const {
    register,
    handleSubmit,
    control,
    formState: { isDirty, isSubmitting, errors },
    reset,
    setValue,
    watch,
  } = useForm<MonitoringFormData>({
    defaultValues: { monitoring: DEFAULTS },
  })

  const monitoring = useWatch({ control, name: 'monitoring' })
  const storeKind: MetricsStoreKind = watch('monitoring.store')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Metrics Monitoring' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Metrics Monitoring')

  useEffect(() => {
    if (settings?.monitoring) {
      reset({ monitoring: settings.monitoring })
    }
  }, [settings, reset])

  const onSubmit = async (data: MonitoringFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success('Monitoring settings saved')
    } catch (err: unknown) {
      const detail =
        err instanceof Error ? err.message : 'Failed to save monitoring settings'
      toast.error(detail)
    }
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Loader2 className="h-8 w-8 animate-spin" />
      </div>
    )
  }

  if (error) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Error</AlertTitle>
        <AlertDescription>Failed to load settings.</AlertDescription>
      </Alert>
    )
  }

  const estimatedMbPerDay = estimateStorageMbPerDay(
    monitoring?.scrape_interval_secs ?? DEFAULTS.scrape_interval_secs
  )

  return (
    <form onSubmit={handleSubmit(onSubmit)} className="space-y-6">
      {/* Overview — metrics collection is always on, controlled per-service */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <BarChart2 className="h-5 w-5" />
            Metrics Collection
          </CardTitle>
          <CardDescription>
            Collect resource and performance metrics from databases, containers,
            and nodes for alerting and dashboards. Enable monitoring per service
            from its detail page — these settings tune how the collected data is
            sampled and retained.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex items-center justify-between rounded-lg border border-border bg-muted/30 px-4 py-3">
            <div className="flex items-center gap-2">
              <Database className="h-4 w-4 text-muted-foreground" />
              <div>
                <p className="text-sm font-medium">Storage backend</p>
                <p className="text-xs text-muted-foreground">
                  Follows the server configuration — set via{' '}
                  <code className="font-mono">TEMPS_CLICKHOUSE_*</code> env vars.
                </p>
              </div>
            </div>
            <span className="rounded-md bg-background px-2.5 py-1 text-xs font-medium text-foreground ring-1 ring-inset ring-border">
              {storeKind === 'click_house' ? 'ClickHouse' : 'TimescaleDB'}
            </span>
          </div>
        </CardContent>
      </Card>

      {/* Scrape interval */}
      <Card>
        <CardHeader>
          <CardTitle>Scrape Interval</CardTitle>
          <CardDescription>
            How often the MetricsScraper collects data from all sources. Lower
            values give higher resolution but increase storage usage.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="w-full sm:w-48">
            <Label htmlFor="scrape-interval">Interval</Label>
            <Select
              value={String(monitoring?.scrape_interval_secs ?? 30)}
              onValueChange={(v) =>
                setValue('monitoring.scrape_interval_secs', Number(v), {
                  shouldDirty: true,
                })
              }
            >
              <SelectTrigger id="scrape-interval" className="mt-1">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="15">15 seconds</SelectItem>
                <SelectItem value="30">30 seconds (default)</SelectItem>
                <SelectItem value="60">60 seconds</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </CardContent>
      </Card>

      {/* Retention tiers */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <HardDrive className="h-5 w-5" />
            Retention
          </CardTitle>
          <CardDescription>
            How long data is kept at each resolution tier. TimescaleDB continuous
            aggregates enforce hourly and daily retention automatically.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <div className="grid gap-6 sm:grid-cols-3">
            <div className="space-y-2">
              <Label htmlFor="retention-raw">Raw data (days)</Label>
              <Input
                id="retention-raw"
                type="number"
                min={1}
                max={30}
                {...register('monitoring.retention_raw_days', {
                  valueAsNumber: true,
                  required: true,
                  min: { value: 1, message: 'Min 1 day' },
                  max: { value: 30, message: 'Max 30 days' },
                })}
              />
              <p className="text-xs text-muted-foreground">
                30-second resolution. Min 1, max 30. Default 7.
              </p>
              {errors.monitoring?.retention_raw_days && (
                <p className="text-xs text-destructive">
                  {errors.monitoring.retention_raw_days.message}
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="retention-hourly">Hourly rollup (days)</Label>
              <Input
                id="retention-hourly"
                type="number"
                min={7}
                max={365}
                {...register('monitoring.retention_hourly_days', {
                  valueAsNumber: true,
                  required: true,
                  min: { value: 7, message: 'Min 7 days' },
                  max: { value: 365, message: 'Max 365 days' },
                })}
              />
              <p className="text-xs text-muted-foreground">
                1-hour resolution. Min 7, max 365. Default 90.
              </p>
              {errors.monitoring?.retention_hourly_days && (
                <p className="text-xs text-destructive">
                  {errors.monitoring.retention_hourly_days.message}
                </p>
              )}
            </div>

            <div className="space-y-2">
              <Label htmlFor="retention-daily">Daily rollup (years)</Label>
              <Input
                id="retention-daily"
                type="number"
                min={1}
                max={10}
                {...register('monitoring.retention_daily_years', {
                  valueAsNumber: true,
                  required: true,
                  min: { value: 1, message: 'Min 1 year' },
                  max: { value: 10, message: 'Max 10 years' },
                })}
              />
              <p className="text-xs text-muted-foreground">
                1-day resolution. Default 2 years.
              </p>
              {errors.monitoring?.retention_daily_years && (
                <p className="text-xs text-destructive">
                  {errors.monitoring.retention_daily_years.message}
                </p>
              )}
            </div>
          </div>

          {/* Estimated storage */}
          <Alert>
            <HardDrive className="h-4 w-4" />
            <AlertTitle>Estimated storage</AlertTitle>
            <AlertDescription>
              Approximately{' '}
              <strong>{estimatedMbPerDay} MB/day</strong> of raw metric data
              based on 5 monitored services, {METRICS_PER_SERVICE} metrics
              each, scraped every{' '}
              {monitoring?.scrape_interval_secs ?? 30}s. Hourly/daily
              rollups add roughly 5% overhead.
            </AlertDescription>
          </Alert>
        </CardContent>
      </Card>

      {isDirty && (
        <div className="sticky bottom-0 bg-background border-t pt-4 pb-2">
          <div className="flex justify-between items-center">
            <p className="text-sm text-muted-foreground">
              You have unsaved changes
            </p>
            <Button type="submit" disabled={isSubmitting}>
              {isSubmitting ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Saving...
                </>
              ) : (
                <>
                  <Save className="mr-2 h-4 w-4" />
                  Save Changes
                </>
              )}
            </Button>
          </div>
        </div>
      )}
    </form>
  )
}
