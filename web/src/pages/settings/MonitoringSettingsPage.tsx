// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { SettingsSection } from '@/components/ui/settings-section'
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
import type {
  MetricsStoreKind,
  MonitoringSettings,
  ObservabilityCompressionSettings,
  ObservabilityRetentionSettings,
  PlatformSettings,
} from '@/api/platformSettings'
import {
  AlertCircle,
  Archive,
  BarChart2,
  Database,
  Globe,
  HardDrive,
  Loader2,
  Save,
  Timer,
} from 'lucide-react'
import { forwardRef, useEffect, type ComponentProps } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'

interface MonitoringFormData {
  monitoring: MonitoringSettings
  observability_compression: ObservabilityCompressionSettings
  observability_retention: ObservabilityRetentionSettings
  geo: GeoFormData
}

/**
 * The editable subset of `GeoSettings`. The read-only refresh metadata and
 * the `effective_*` values come straight from `settings.geo` and are rendered
 * without going through the form.
 *
 * `maxmind_license_key` is always submitted as a string: empty means "keep the
 * stored key", matching the server's blank-preserves contract.
 */
interface GeoFormData {
  refresh_interval_hours: number | null
  stale_lookup_days: number | null
  maxmind_license_key: string
}

const DEFAULTS: MonitoringSettings = {
  enabled: false,
  store: 'timescale_db',
  scrape_interval_secs: 30,
  retention_raw_days: 7,
  retention_hourly_days: 90,
  retention_daily_years: 2,
  clickhouse_url_set: false,
  clickhouse_url: null,
}

const COMPRESSION_DEFAULTS: ObservabilityCompressionSettings = {
  proxy_logs_after_hours: 24,
  otel_spans_after_hours: 24,
}

const RETENTION_DEFAULTS: ObservabilityRetentionSettings = {
  proxy_logs_days: 30,
  otel_spans_days: 90,
  otel_logs_days: 90,
  otel_metrics_days: 90,
}

// `null` means "use the server default" for both knobs, which is also what
// clearing the input produces.
const GEO_DEFAULTS: GeoFormData = {
  refresh_interval_hours: null,
  stale_lookup_days: null,
  maxmind_license_key: '',
}

// Bytes per raw metric row (approximate: time 8 + source_kind 12 + source_id 4 +
// name 20 + value 8 + labels 32 = ~84 bytes; round up to 100 for overhead)
const BYTES_PER_ROW = 100
// Approximate number of metrics tracked per monitored service
const METRICS_PER_SERVICE = 15

type DurationUnit = 'hours' | 'days' | 'years'

interface DurationInputProps extends ComponentProps<'input'> {
  unit: DurationUnit
}

const DurationInput = forwardRef<HTMLInputElement, DurationInputProps>(
  ({ unit, ...props }, ref) => (
    <div className="relative text-base md:text-sm">
      <Input
        ref={ref}
        type="number"
        className="pr-20 tabular-nums"
        {...props}
      />
      <span className="pointer-events-none absolute inset-y-0 right-8 flex items-center text-muted-foreground">
        {unit}
      </span>
    </div>
  )
)
DurationInput.displayName = 'DurationInput'

/**
 * A cleared number input yields `''`, which must become `null` ("use the
 * default") rather than `NaN` — the server rejects a non-number and would
 * otherwise report a validation error for a field the admin just cleared.
 */
function emptyToNull(value: unknown): number | null {
  if (value === '' || value === null || value === undefined) return null
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : null
}

function describeGeoSource(source: string | null): string {
  if (source === 'maxmind_official') return 'MaxMind (license key)'
  if (source === 'bundled_github') return 'the bundled repository copy'
  return 'an unknown source'
}

/**
 * The one-line answer to "is my geolocation data current?", built from the
 * refresh metadata the server records. Never renders an empty state: an
 * instance that has not refreshed yet says so explicitly.
 */
function describeGeoStatus(geo: PlatformSettings['geo'] | undefined): {
  headline: string
  detail: string
} {
  if (!geo) {
    return {
      headline: 'Geolocation status unavailable',
      detail:
        'This server did not report geolocation state. Restart it, or check the server logs.',
    }
  }

  const source = describeGeoSource(geo.source)

  if (!geo.last_check_at) {
    return {
      headline: 'No refresh has run yet',
      detail: `The scheduled job checks every ${geo.effective_refresh_interval_hours} hours. Until then, lookups use whatever database is on disk.`,
    }
  }

  const refreshed = geo.last_refreshed_at
    ? `last updated ${new Date(geo.last_refreshed_at).toLocaleString()}`
    : 'never updated by this instance'

  // Not refreshing by configuration, not by failure. Say so, with the fix —
  // otherwise "last checked today, never updated" reads as a broken job.
  if (geo.last_check_status === 'skipped_no_license_key') {
    return {
      headline: 'Automatic refreshes are not running',
      detail: `No MaxMind license key is configured, so the scheduled check (every ${geo.effective_refresh_interval_hours} hours) downloads nothing — lookups keep using the database already on disk (${refreshed}). Add a license key below to enable refreshes.`,
    }
  }

  return {
    headline:
      geo.last_check_status === 'error'
        ? 'Last refresh check failed'
        : `Database downloaded from ${source}`,
    detail: `Checked ${new Date(geo.last_check_at).toLocaleString()}, ${refreshed}. Refreshing every ${geo.effective_refresh_interval_hours} hours; stored lookups expire after ${geo.effective_stale_lookup_days} days.`,
  }
}

function estimateStorageMbPerDay(
  scrapeIntervalSecs: number,
  monitoredServices: number
): number {
  const scrapesPerDay = Math.floor(
    (24 * 3600) / Math.max(scrapeIntervalSecs, 15)
  )
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
  } = useForm<MonitoringFormData>({
    defaultValues: {
      monitoring: DEFAULTS,
      observability_compression: COMPRESSION_DEFAULTS,
      observability_retention: RETENTION_DEFAULTS,
      geo: GEO_DEFAULTS,
    },
  })

  const removeLicenseKey = useUpdateSettings()
  const monitoring = useWatch({ control, name: 'monitoring' })
  const storeKind: MetricsStoreKind = monitoring?.store ?? DEFAULTS.store
  // The backend the runtime actually writes to, reconciled server-side with
  // the TEMPS_CLICKHOUSE_* env vars. Falls back to the configured store if the
  // server didn't report it (older binaries).
  const effectiveStore: MetricsStoreKind =
    settings?.effective_metrics_store ?? storeKind
  const storeMismatch =
    storeKind === 'click_house' && effectiveStore !== 'click_house'
  const effectiveObservabilityStore: MetricsStoreKind =
    settings?.effective_observability_store ?? 'timescale_db'

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Metrics Monitoring' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Metrics Monitoring')

  useEffect(() => {
    if (settings?.monitoring) {
      reset({
        monitoring: settings.monitoring,
        observability_compression:
          settings.observability_compression ?? COMPRESSION_DEFAULTS,
        observability_retention:
          settings.observability_retention ?? RETENTION_DEFAULTS,
        geo: {
          refresh_interval_hours:
            settings.geo?.refresh_interval_hours ?? null,
          stale_lookup_days: settings.geo?.stale_lookup_days ?? null,
          // Never seeded from the server — the key is write-only, and a blank
          // field is what preserves the stored one.
          maxmind_license_key: '',
        },
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: MonitoringFormData) => {
    try {
      const licenseKey = data.geo.maxmind_license_key.trim()
      await updateSettings.mutateAsync({
        monitoring: data.monitoring,
        observability_compression: data.observability_compression,
        observability_retention: data.observability_retention,
        geo: {
          // Round-trip the server's read-only view so the metadata and the
          // masked key flag are not dropped from the merged body.
          ...(settings?.geo ?? ({} as PlatformSettings['geo'])),
          refresh_interval_hours: data.geo.refresh_interval_hours,
          stale_lookup_days: data.geo.stale_lookup_days,
          // Omit the key entirely when the field was left blank, so a save
          // from this page can never wipe a stored key.
          ...(licenseKey ? { maxmind_license_key: licenseKey } : {}),
        },
      })
      // Clear the key field, not the whole form: a submitted key is now
      // stored, and leaving it in the input would re-submit it on the next
      // save (and keep it in memory for as long as the page is open).
      reset({ ...data, geo: { ...data.geo, maxmind_license_key: '' } })
      toast.success('Monitoring settings saved')
    } catch (err: unknown) {
      const detail =
        err instanceof Error
          ? err.message
          : 'Failed to save monitoring settings'
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

  const geo = settings?.geo
  const geoStatus = describeGeoStatus(geo)

  const handleRemoveLicenseKey = async () => {
    try {
      await removeLicenseKey.mutateAsync({
        geo: {
          ...(geo ?? ({} as PlatformSettings['geo'])),
          clear_maxmind_license_key: true,
        },
      })
      setValue('geo.maxmind_license_key', '', { shouldDirty: false })
      toast.success(
        'MaxMind license key removed; refreshes now use the bundled database'
      )
    } catch (err: unknown) {
      toast.error(
        err instanceof Error ? err.message : 'Failed to remove the license key'
      )
    }
  }

  const monitoredServicesCount = settings?.monitored_services_count
  const estimatedMbPerDay =
    monitoredServicesCount == null
      ? null
      : estimateStorageMbPerDay(
          monitoring?.scrape_interval_secs ?? DEFAULTS.scrape_interval_secs,
          monitoredServicesCount
        )

  return (
    <form onSubmit={handleSubmit(onSubmit)} className="space-y-6">
      {/* Overview — metrics collection is always on, controlled per-service */}
      <SettingsSection
        title="Metrics Collection"
        icon={BarChart2}
        description="Collect resource and performance metrics from databases, containers, and nodes for alerting and dashboards. Enable monitoring per service from its detail page — these settings tune how the collected data is sampled and retained."
      >
        <div className="space-y-3">
          <div className="flex flex-col gap-3 rounded-lg border border-border bg-muted/30 px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-center gap-2">
              <Database className="h-4 w-4 text-muted-foreground" />
              <div>
                <p className="text-sm font-medium">Storage backend</p>
                <p className="text-xs text-muted-foreground">
                  ClickHouse is used only when this setting selects it{' '}
                  <em>and</em> the{' '}
                  <code className="font-mono">TEMPS_CLICKHOUSE_*</code> env vars
                  are set on the server; otherwise metrics fall back to
                  TimescaleDB. Changing the backend takes effect after the
                  server restarts.
                </p>
              </div>
            </div>
            <div className="flex items-center gap-2">
              <span className="rounded-md bg-background px-2.5 py-1 text-xs font-medium text-foreground ring-1 ring-inset ring-border">
                Active:{' '}
                {effectiveStore === 'click_house'
                  ? 'ClickHouse'
                  : 'TimescaleDB'}
              </span>
              <Select
                value={storeKind}
                onValueChange={(v) =>
                  setValue('monitoring.store', v as MetricsStoreKind, {
                    shouldDirty: true,
                  })
                }
              >
                <SelectTrigger className="w-full sm:w-[160px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="timescale_db">TimescaleDB</SelectItem>
                  <SelectItem value="click_house">ClickHouse</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>

          {/* The DB toggle says ClickHouse but the runtime fell back to
              TimescaleDB because the env vars aren't fully configured. Surface
              the divergence so the badge above isn't silently misleading. */}
          {storeMismatch && (
            <Alert variant="destructive">
              <AlertCircle className="h-4 w-4" />
              <AlertTitle>Configured backend is not active</AlertTitle>
              <AlertDescription>
                The metrics store is set to <strong>ClickHouse</strong>, but the
                server&apos;s{' '}
                <code className="font-mono">TEMPS_CLICKHOUSE_*</code>{' '}
                environment variables are not fully configured, so metrics are
                being written to <strong>TimescaleDB</strong>. Set the
                ClickHouse env vars on the control plane and restart, or switch
                the store back to TimescaleDB to clear this warning.
              </AlertDescription>
            </Alert>
          )}
        </div>
      </SettingsSection>

      {/* Immutable proxy logs and spans share one operational pattern: write
          once, then compress closed Timescale chunks. ClickHouse compresses
          parts automatically and therefore has no age-based policy. */}
      <SettingsSection
        title="Telemetry Compression"
        icon={Archive}
        description="Reduce storage used by immutable proxy request logs and OpenTelemetry spans after their active ingest window closes."
      >
        <div className="space-y-4">
          <span className="inline-flex rounded-md bg-background px-2.5 py-1 text-xs font-medium text-foreground ring-1 ring-inset ring-border">
            {effectiveObservabilityStore === 'click_house'
              ? 'ClickHouse · automatic'
              : 'TimescaleDB · scheduled'}
          </span>
          {effectiveObservabilityStore === 'click_house' ? (
            <div className="rounded-lg border border-border bg-muted/30 px-4 py-4">
              <div className="flex items-start gap-3">
                <Database className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
                <div className="space-y-1">
                  <p className="text-sm font-medium">
                    Compression is managed automatically
                  </p>
                  <p className="text-sm text-muted-foreground">
                    ClickHouse compresses data parts as they are written and
                    merged using column-specific Delta, Gorilla, and ZSTD
                    codecs. There is no age-based compression delay to tune.
                  </p>
                </div>
              </div>
            </div>
          ) : (
            <div className="space-y-5">
              <div className="grid gap-5 sm:grid-cols-2">
                <div className="space-y-2">
                  <Label htmlFor="proxy-compression-delay">
                    Proxy logs delay
                  </Label>
                  <DurationInput
                    id="proxy-compression-delay"
                    unit="hours"
                    min={1}
                    max={720}
                    {...register(
                      'observability_compression.proxy_logs_after_hours',
                      {
                        valueAsNumber: true,
                        required: 'Enter a compression delay',
                        min: { value: 1, message: 'Min 1 hour' },
                        max: { value: 720, message: 'Max 720 hours' },
                      }
                    )}
                  />
                  <p className="text-xs text-muted-foreground">
                    Compresses closed proxy-log chunks once their newest row is
                    this old. It does not delete logs. Default 24 hours; maximum
                    30 days.
                  </p>
                  {errors.observability_compression?.proxy_logs_after_hours && (
                    <p className="text-xs text-destructive">
                      {
                        errors.observability_compression.proxy_logs_after_hours
                          .message
                      }
                    </p>
                  )}
                </div>

                <div className="space-y-2">
                  <Label htmlFor="span-compression-delay">
                    OTel spans delay
                  </Label>
                  <DurationInput
                    id="span-compression-delay"
                    unit="hours"
                    min={1}
                    max={2160}
                    {...register(
                      'observability_compression.otel_spans_after_hours',
                      {
                        valueAsNumber: true,
                        required: 'Enter a compression delay',
                        min: { value: 1, message: 'Min 1 hour' },
                        max: { value: 2160, message: 'Max 2160 hours' },
                      }
                    )}
                  />
                  <p className="text-xs text-muted-foreground">
                    Compresses closed span chunks once their newest row is this
                    old. It does not delete traces. Default 24 hours; maximum 90
                    days.
                  </p>
                  {errors.observability_compression?.otel_spans_after_hours && (
                    <p className="text-xs text-destructive">
                      {
                        errors.observability_compression.otel_spans_after_hours
                          .message
                      }
                    </p>
                  )}
                </div>
              </div>

              <p className="border-l-2 border-border pl-3 text-xs leading-5 text-muted-foreground">
                Set the delay beyond your normal late-arrival window. Lower
                values reclaim disk sooner; higher values keep more recent
                chunks in row storage for operational queries.
              </p>
            </div>
          )}
        </div>
      </SettingsSection>

      {/* Scrape interval */}
      <SettingsSection
        title="Scrape Interval"
        icon={Timer}
        description="How often the MetricsScraper collects data from all sources. Lower values give higher resolution but increase storage usage."
      >
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
          <p className="mt-2 text-xs leading-5 text-muted-foreground">
            Controls how often Temps samples CPU, memory, database, container,
            and node metrics. Shorter intervals show finer detail but create
            more raw metric rows.
          </p>
        </div>
      </SettingsSection>

      {/* Retention spans every observability signal, not only resource metrics. */}
      <SettingsSection
        title="Data Retention"
        icon={HardDrive}
        description="Control how long resource metrics, proxy request logs, traces, OpenTelemetry logs, and OpenTelemetry metrics remain available."
      >
        <div className="space-y-8">
          <span className="inline-flex rounded-md bg-background px-2.5 py-1 text-xs font-medium text-foreground ring-1 ring-inset ring-border">
            All telemetry
          </span>
          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Retention permanently deletes old data</AlertTitle>
            <AlertDescription>
              Each value is an independent policy for one table or resolution
              tier. Shortening a window makes data beyond that age eligible for
              deletion on the next background policy run. Compression is
              different: it reduces storage without deleting rows.
            </AlertDescription>
          </Alert>

          <section className="space-y-4">
            <div>
              <h3 className="text-sm font-semibold">Resource metrics</h3>
              <p className="text-xs text-muted-foreground">
                Samples and rollups used by service and node dashboards.
              </p>
            </div>
            {effectiveStore === 'click_house' ? (
              <div className="rounded-lg border bg-muted/30 px-4 py-3">
                <p className="text-sm font-medium">
                  ClickHouse retention is managed by table TTL
                </p>
                <p className="mt-1 text-xs text-muted-foreground">
                  Resource metric rows are retained for 90 days, with rollups
                  calculated at query time. Rows older than 90 days are
                  permanently deleted.
                </p>
              </div>
            ) : (
              <div className="grid gap-4 lg:grid-cols-3">
                <div className="space-y-3 rounded-lg border p-4">
                  <Label htmlFor="retention-raw">Raw data (days)</Label>
                  <p className="min-h-10 text-xs leading-5 text-muted-foreground">
                    Stores every original sample at the selected scrape interval
                    for detailed recent charts and precise incident
                    investigation.
                  </p>
                  <DurationInput
                    id="retention-raw"
                    unit="days"
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
                    Raw samples older than this are permanently deleted. Hourly
                    and daily summaries remain. Range 1–30 days; default 7.
                  </p>
                  {errors.monitoring?.retention_raw_days && (
                    <p className="text-xs text-destructive">
                      {errors.monitoring.retention_raw_days.message}
                    </p>
                  )}
                </div>

                <div className="space-y-3 rounded-lg border p-4">
                  <Label htmlFor="retention-hourly">Hourly rollup (days)</Label>
                  <p className="min-h-10 text-xs leading-5 text-muted-foreground">
                    Creates an hourly aggregate for each metric to preserve
                    medium-term trends after individual raw samples are deleted.
                  </p>
                  <DurationInput
                    id="retention-hourly"
                    unit="days"
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
                    Hourly aggregates older than this are permanently deleted.
                    Daily summaries remain. Range 7–365 days; default 90.
                  </p>
                  {errors.monitoring?.retention_hourly_days && (
                    <p className="text-xs text-destructive">
                      {errors.monitoring.retention_hourly_days.message}
                    </p>
                  )}
                </div>

                <div className="space-y-3 rounded-lg border p-4">
                  <Label htmlFor="retention-daily">Daily rollup (years)</Label>
                  <p className="min-h-10 text-xs leading-5 text-muted-foreground">
                    Creates a daily aggregate for each metric for capacity
                    planning, seasonality, and long-term historical trends.
                  </p>
                  <DurationInput
                    id="retention-daily"
                    unit="years"
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
                    Daily aggregates older than this are permanently deleted.
                    This is the longest-lived metrics tier. Range 1–10 years;
                    default 2.
                  </p>
                  {errors.monitoring?.retention_daily_years && (
                    <p className="text-xs text-destructive">
                      {errors.monitoring.retention_daily_years.message}
                    </p>
                  )}
                </div>
              </div>
            )}

            <Alert>
              <HardDrive className="h-4 w-4" />
              <AlertTitle>
                {effectiveStore === 'click_house'
                  ? 'Estimated metric ingest'
                  : 'Estimated metric storage'}
              </AlertTitle>
              <AlertDescription>
                {estimatedMbPerDay == null ? (
                  'The estimate is unavailable because the monitored service count could not be loaded.'
                ) : (
                  <>
                    Approximately <strong>{estimatedMbPerDay} MB/day</strong> of
                    raw metric data based on {monitoredServicesCount} monitored{' '}
                    {monitoredServicesCount === 1 ? 'service' : 'services'},{' '}
                    {METRICS_PER_SERVICE} metrics each, scraped every{' '}
                    {monitoring?.scrape_interval_secs ?? 30}s.{' '}
                    {effectiveStore === 'click_house'
                      ? 'This is ingest volume before ClickHouse compression; actual disk usage depends on codecs and data cardinality.'
                      : 'Hourly/daily rollups add roughly 5% overhead.'}
                  </>
                )}
              </AlertDescription>
            </Alert>
          </section>

          <div className="border-t" />

          <section className="space-y-4">
            <div>
              <h3 className="text-sm font-semibold">Logs and traces</h3>
              <p className="text-xs text-muted-foreground">
                Each signal has its own table and retention window, so
                high-volume request logs can be deleted sooner than traces or
                application telemetry. The active storage backend permanently
                deletes expired rows in the background.
              </p>
            </div>

            {effectiveObservabilityStore === 'click_house' && (
              <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
                <div className="rounded-lg border bg-muted/30 p-4">
                  <div className="flex items-center justify-between gap-3">
                    <p className="text-sm font-medium">Proxy request logs</p>
                    <code className="rounded bg-background px-2 py-0.5 text-[11px] text-muted-foreground ring-1 ring-inset ring-border">
                      proxy_logs
                    </code>
                  </div>
                  <p className="mt-2 text-xs leading-5 text-muted-foreground">
                    One row per proxied HTTP request, including route, status,
                    duration, and request metadata. ClickHouse permanently
                    deletes rows older than 30 days using its native TTL.
                  </p>
                </div>
                <div className="rounded-lg border bg-muted/30 p-4">
                  <div className="flex items-center justify-between gap-3">
                    <p className="text-sm font-medium">OTel metric points</p>
                    <code className="rounded bg-background px-2 py-0.5 text-[11px] text-muted-foreground ring-1 ring-inset ring-border">
                      metrics
                    </code>
                  </div>
                  <p className="mt-2 text-xs leading-5 text-muted-foreground">
                    Application metrics received over OTLP. ClickHouse
                    permanently deletes rows older than 90 days using its native
                    TTL.
                  </p>
                </div>
                <div className="rounded-lg border bg-muted/30 p-4">
                  <div className="flex items-center justify-between gap-3">
                    <p className="text-sm font-medium">OTel spans / traces</p>
                    <code className="rounded bg-background px-2 py-0.5 text-[11px] text-muted-foreground ring-1 ring-inset ring-border">
                      spans
                    </code>
                  </div>
                  <p className="mt-2 text-xs leading-5 text-muted-foreground">
                    Individual operations that compose a distributed trace,
                    including timings, errors, attributes, and service links.
                    ClickHouse permanently deletes rows older than 90 days using
                    its native TTL.
                  </p>
                </div>
              </div>
            )}

            <div className="grid gap-4 md:grid-cols-2">
              {effectiveObservabilityStore !== 'click_house' && (
                <>
                  <div className="space-y-3 rounded-lg border p-4">
                    <div className="flex items-center justify-between gap-3">
                      <Label htmlFor="retention-proxy-logs">
                        Proxy request logs
                      </Label>
                      <code className="rounded bg-muted px-2 py-0.5 text-[11px] text-muted-foreground">
                        proxy_logs
                      </code>
                    </div>
                    <p className="min-h-10 text-xs leading-5 text-muted-foreground">
                      Stores a separate log entry for every HTTP request handled
                      by the proxy. Use these entries to investigate traffic,
                      status codes, and slow requests.
                    </p>
                    <DurationInput
                      id="retention-proxy-logs"
                      unit="days"
                      min={1}
                      max={3650}
                      {...register('observability_retention.proxy_logs_days', {
                        valueAsNumber: true,
                        required: true,
                        min: { value: 1, message: 'Min 1 day' },
                        max: { value: 3650, message: 'Max 3650 days' },
                      })}
                    />
                    <p className="text-xs text-muted-foreground">
                      Proxy log entries older than this are permanently deleted.
                      Range 1–3650 days; default 30.
                    </p>
                    {errors.observability_retention?.proxy_logs_days && (
                      <p className="text-xs text-destructive">
                        {errors.observability_retention.proxy_logs_days.message}
                      </p>
                    )}
                  </div>

                  <div className="space-y-3 rounded-lg border p-4">
                    <div className="flex items-center justify-between gap-3">
                      <Label htmlFor="retention-otel-spans">
                        OTel spans / traces
                      </Label>
                      <code className="rounded bg-muted px-2 py-0.5 text-[11px] text-muted-foreground">
                        otel_spans
                      </code>
                    </div>
                    <p className="min-h-10 text-xs leading-5 text-muted-foreground">
                      Stores every received OTel span used to reconstruct trace
                      waterfalls, follow requests across services, and inspect
                      timings, errors, events, and attributes.
                    </p>
                    <DurationInput
                      id="retention-otel-spans"
                      unit="days"
                      min={1}
                      max={3650}
                      {...register('observability_retention.otel_spans_days', {
                        valueAsNumber: true,
                        required: true,
                        min: { value: 1, message: 'Min 1 day' },
                        max: { value: 3650, message: 'Max 3650 days' },
                      })}
                    />
                    <p className="text-xs text-muted-foreground">
                      Spans older than this are permanently deleted and
                      disappear from trace search and detail views. Range 1–3650
                      days; default 90.
                    </p>
                    {errors.observability_retention?.otel_spans_days && (
                      <p className="text-xs text-destructive">
                        {errors.observability_retention.otel_spans_days.message}
                      </p>
                    )}
                  </div>
                </>
              )}

              <div className="space-y-3 rounded-lg border p-4">
                <div className="flex items-center justify-between gap-3">
                  <Label htmlFor="retention-otel-logs">OTel log records</Label>
                  <code className="rounded bg-muted px-2 py-0.5 text-[11px] text-muted-foreground">
                    otel_log_events
                  </code>
                </div>
                <p className="min-h-10 text-xs leading-5 text-muted-foreground">
                  Stores every structured log received over OTLP, including
                  severity, body, attributes, and trace correlation. This does
                  not control Docker container log files.
                </p>
                <DurationInput
                  id="retention-otel-logs"
                  unit="days"
                  min={1}
                  max={3650}
                  {...register('observability_retention.otel_logs_days', {
                    valueAsNumber: true,
                    required: true,
                    min: { value: 1, message: 'Min 1 day' },
                    max: { value: 3650, message: 'Max 3650 days' },
                  })}
                />
                <p className="text-xs text-muted-foreground">
                  OTel log records older than this are permanently deleted.
                  Range 1–3650 days; default 90.
                </p>
                {errors.observability_retention?.otel_logs_days && (
                  <p className="text-xs text-destructive">
                    {errors.observability_retention.otel_logs_days.message}
                  </p>
                )}
              </div>

              {effectiveObservabilityStore !== 'click_house' && (
                <div className="space-y-3 rounded-lg border p-4">
                  <div className="flex items-center justify-between gap-3">
                    <Label htmlFor="retention-otel-metrics">
                      OTel metric points
                    </Label>
                    <code className="rounded bg-muted px-2 py-0.5 text-[11px] text-muted-foreground">
                      otel_metrics
                    </code>
                  </div>
                  <p className="min-h-10 text-xs leading-5 text-muted-foreground">
                    Stores application metric points sent by OpenTelemetry SDKs
                    and collectors. These are separate from the Temps-scraped
                    resource metrics configured above.
                  </p>
                  <DurationInput
                    id="retention-otel-metrics"
                    unit="days"
                    min={1}
                    max={3650}
                    {...register('observability_retention.otel_metrics_days', {
                      valueAsNumber: true,
                      required: true,
                      min: { value: 1, message: 'Min 1 day' },
                      max: { value: 3650, message: 'Max 3650 days' },
                    })}
                  />
                  <p className="text-xs text-muted-foreground">
                    OTel metric points older than this are permanently deleted.
                    Range 1–3650 days; default 90.
                  </p>
                  {errors.observability_retention?.otel_metrics_days && (
                    <p className="text-xs text-destructive">
                      {errors.observability_retention.otel_metrics_days.message}
                    </p>
                  )}
                </div>
              )}
            </div>
          </section>
        </div>
      </SettingsSection>

      {/* Geolocation feeds country/city onto proxy logs, analytics sessions
          and audit entries, so how fresh its database is decides how correct
          every one of those reads. */}
      <SettingsSection
        title="Geolocation Database"
        icon={Globe}
        description="Keep the MaxMind GeoLite2 city database current. With a free MaxMind license key Temps downloads the latest build itself; without one it falls back to the copy shipped with the repository, which is only as current as the release."
      >
        <div className="space-y-4">
          <div className="rounded-lg border border-border bg-muted/30 px-4 py-3">
            <p className="text-sm font-medium">
              {geoStatus.headline}
            </p>
            <p className="mt-1 text-xs text-muted-foreground">
              {geoStatus.detail}
            </p>
          </div>

          {geo?.last_check_status === 'error' && (
            <Alert variant="destructive">
              <AlertCircle className="h-4 w-4" />
              <AlertTitle>Last refresh failed</AlertTitle>
              <AlertDescription>
                {geo.last_error ??
                  'No reason was recorded. Check the server logs.'}{' '}
                The previously downloaded database is still being used for
                lookups.
              </AlertDescription>
            </Alert>
          )}

          <div className="space-y-3 rounded-lg border p-4">
            <Label htmlFor="geo-license-key">MaxMind license key</Label>
            <p className="text-xs leading-5 text-muted-foreground">
              Create one free at{' '}
              <code className="font-mono">
                maxmind.com → My License Keys
              </code>
              . Stored encrypted; it is never shown again after saving.
            </p>
            <Input
              id="geo-license-key"
              type="password"
              autoComplete="new-password"
              placeholder={
                geo?.maxmind_license_key_saved
                  ? 'Leave blank to keep current'
                  : 'No key configured — downloads use the bundled database'
              }
              {...register('geo.maxmind_license_key')}
            />
            {geo?.maxmind_license_key_saved && (
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={removeLicenseKey.isPending}
                onClick={handleRemoveLicenseKey}
              >
                {removeLicenseKey.isPending
                  ? 'Removing...'
                  : 'Remove stored key'}
              </Button>
            )}
          </div>

          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <div className="space-y-3 rounded-lg border p-4">
              <Label htmlFor="geo-refresh-interval">Refresh interval</Label>
              <p className="min-h-10 text-xs leading-5 text-muted-foreground">
                How often the scheduled job re-downloads the database. MaxMind
                publishes GeoLite2 twice a week.
              </p>
              <DurationInput
                id="geo-refresh-interval"
                unit="hours"
                min={1}
                max={8760}
                placeholder={String(
                  geo?.effective_refresh_interval_hours ?? 24
                )}
                {...register('geo.refresh_interval_hours', {
                  // `null` (a cleared field) is valid and means "default", so
                  // this cannot use the built-in min/max rules.
                  setValueAs: emptyToNull,
                  validate: (value) =>
                    value === null ||
                    (value >= 1 && value <= 8760) ||
                    'Must be 1–8760 hours, or empty for the default',
                })}
              />
              <p className="text-xs text-muted-foreground">
                Leave empty for the default of 24 hours. Range 1–8760.
              </p>
              {errors.geo?.refresh_interval_hours && (
                <p className="text-xs text-destructive">
                  {errors.geo.refresh_interval_hours.message}
                </p>
              )}
            </div>

            <div className="space-y-3 rounded-lg border p-4">
              <Label htmlFor="geo-stale-lookup">Stored lookup lifetime</Label>
              <p className="min-h-10 text-xs leading-5 text-muted-foreground">
                A stored IP → location row older than this is re-resolved on
                its next lookup, so an IP reassigned to another city stops
                reporting the old one.
              </p>
              <DurationInput
                id="geo-stale-lookup"
                unit="days"
                min={1}
                max={3650}
                placeholder={String(geo?.effective_stale_lookup_days ?? 30)}
                {...register('geo.stale_lookup_days', {
                  setValueAs: emptyToNull,
                  validate: (value) =>
                    value === null ||
                    (value >= 1 && value <= 3650) ||
                    'Must be 1–3650 days, or empty for the default',
                })}
              />
              <p className="text-xs text-muted-foreground">
                Leave empty for the default of 30 days. Range 1–3650.
              </p>
              {errors.geo?.stale_lookup_days && (
                <p className="text-xs text-destructive">
                  {errors.geo.stale_lookup_days.message}
                </p>
              )}
            </div>
          </div>
        </div>
      </SettingsSection>

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
