import { ProjectResponse } from '@/api/client'
import type {
  AnomalyAlgorithm,
  Comparator,
  Direction,
  OtelMetricAlertRuleResponse,
  Seasonality,
} from '@/api/client'
// REGEN: bun run openapi-ts — these builders/types come from the new
// /otel/alerts endpoints (operationIds get_alert / create_alert / update_alert).
// They are absent from the committed SDK; imported here so the form is
// regen-ready instead of hand-rolling fetch (project rule).
import {
  createAlertMutation,
  getAlertOptions,
  listMetricNamesOptions,
  queryMetricsOptions,
  updateAlertMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { SearchableSelect } from '@/components/ui/searchable-select'
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
import { DebugChat } from '@/components/ai/DebugChat'
import { AGGREGATIONS, formatMetricValue } from '@/components/metrics/metric-format'
import { cn } from '@/lib/utils'
import { AnomalyBacktest } from '@/components/metrics/AnomalyBacktest'
import {
  GroupByBuilder,
  LabelFilterBuilder,
  labelFiltersToTuples,
  MAX_GROUP_BY_KEYS,
  tuplesToLabelFilters,
} from '@/components/metrics/LabelFilterBuilder'
import {
  AlertStateBadge,
  ANOMALY_ALGORITHMS,
  ANOMALY_MIN_HISTORY_DAYS,
  COMPARATORS,
  DETECTION_KINDS,
  DIRECTIONS,
  presetForDeviations,
  SEASONALITIES,
  SENSITIVITY_PRESETS,
  SEVERITIES,
} from '@/components/metrics/alert-format'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  ChevronDown,
  SlidersHorizontal,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'

interface MetricAlertFormProps {
  project: ProjectResponse
}

const AGGREGATION_VALUES = AGGREGATIONS.map((a) => a.value) as [
  string,
  ...string[],
]
const COMPARATOR_VALUES = COMPARATORS.map((c) => c.value) as [
  string,
  ...string[],
]
const SEVERITY_VALUES = SEVERITIES.map((s) => s.value) as [string, ...string[]]
const ALGORITHM_VALUES = ANOMALY_ALGORITHMS.map((a) => a.value) as [
  string,
  ...string[],
]
const DIRECTION_VALUES = DIRECTIONS.map((d) => d.value) as [string, ...string[]]
const SEASONALITY_VALUES = SEASONALITIES.map((s) => s.value) as [
  string,
  ...string[],
]

// Flat schema: comparator/threshold (static) and the anomaly knobs always carry
// form values (with defaults); `onSubmit` builds the right `detection_config`
// variant from `detection_kind`, and the backend validates the final config.
const alertSchema = z.object({
  name: z.string().min(1, 'Name is required').max(200, 'Name is too long'),
  metric_name: z.string().min(1, 'Pick a metric'),
  aggregation: z.enum(AGGREGATION_VALUES),
  detection_kind: z.enum(['static', 'anomaly']),
  comparator: z.enum(COMPARATOR_VALUES),
  threshold: z.number({ message: 'Threshold must be a number' }),
  algorithm: z.enum(ALGORITHM_VALUES),
  deviations: z
    .number({ message: 'Sensitivity must be a number' })
    .positive('Sensitivity must be greater than 0'),
  direction: z.enum(DIRECTION_VALUES),
  seasonality: z.enum(SEASONALITY_VALUES),
  window_secs: z
    .number({ message: 'Window must be a number' })
    .int()
    .positive('Window must be greater than 0'),
  for_duration_secs: z
    .number({ message: 'Duration must be a number' })
    .int()
    .positive('Duration must be greater than 0'),
  severity: z.enum(SEVERITY_VALUES),
  enabled: z.boolean(),
  label_filters: z.array(z.object({ key: z.string(), value: z.string() })),
  group_by: z.array(z.string()).max(MAX_GROUP_BY_KEYS),
  dynamic_alerts: z.boolean(),
  max_series: z
    .number({ message: 'Max series must be a number' })
    .int()
    .min(1, 'Max series must be at least 1')
    .max(100, 'Max series cannot exceed 100'),
  grouped_notification_threshold: z
    .number({ message: 'Threshold must be a number' })
    .int()
    .min(1, 'Threshold must be at least 1')
    .max(1000, 'Threshold cannot exceed 1000'),
})

type AlertFormData = z.infer<typeof alertSchema>

/** Narrow a stored token into a form enum union, falling back to a default. */
function coerce<T extends readonly string[]>(
  values: T,
  value: string,
  fallback: T[number],
): T[number] {
  return (values as readonly string[]).includes(value)
    ? (value as T[number])
    : fallback
}

function emptyDefaults(): AlertFormData {
  return {
    name: '',
    metric_name: '',
    aggregation: 'avg',
    detection_kind: 'static',
    comparator: 'gt',
    threshold: 0,
    algorithm: 'robust',
    deviations: 3,
    direction: 'both',
    seasonality: 'none',
    window_secs: 300,
    // Must be > 0 (backend + Zod reject 0); default to one eval interval.
    for_duration_secs: 60,
    severity: 'warning',
    enabled: true,
    label_filters: [],
    group_by: [],
    dynamic_alerts: false,
    max_series: 20,
    grouped_notification_threshold: 5,
  }
}

interface AlertFormBodyProps {
  project: ProjectResponse
  isEditing: boolean
  id: number
  /** The rule being edited, already loaded by the parent (null when creating). */
  existing: OtelMetricAlertRuleResponse | null
}

/**
 * The actual form. Mounted only once `existing` is resolved (the parent gates on
 * loading and remounts via `key`), so `useForm` initializes with the correct
 * values from the start. This is deliberate: initializing with placeholder
 * values and then resetting via the `values` prop drops Radix `Select` values
 * whose value changes during the reset (e.g. aggregation, detection kind).
 */
function AlertFormBody({ project, isEditing, id, existing }: AlertFormBodyProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const namesQuery = useQuery({
    ...listMetricNamesOptions({ path: { project_id: project.id } }),
    enabled: !!project.id,
  })
  const metricOptions = useMemo(
    () =>
      (namesQuery.data?.names ?? []).map((n) => ({
        value: n,
        label: n,
      })),
    [namesQuery.data],
  )

  const defaultValues = useMemo<AlertFormData>(() => {
    if (existing) {
      // Unpack the typed detector union into the flat form fields.
      const cfg = existing.detection_config
      const isStatic = cfg.kind === 'static'
      const isAnomaly = cfg.kind === 'anomaly'
      return {
        name: existing.name,
        metric_name: existing.metric_name,
        aggregation: coerce(AGGREGATION_VALUES, existing.aggregation, 'avg'),
        detection_kind: isAnomaly ? 'anomaly' : 'static',
        comparator: coerce(
          COMPARATOR_VALUES,
          isStatic ? cfg.comparator : 'gt',
          'gt',
        ),
        threshold: isStatic ? cfg.threshold : 0,
        algorithm: coerce(
          ALGORITHM_VALUES,
          isAnomaly ? (cfg.algorithm ?? 'robust') : 'robust',
          'robust',
        ),
        deviations: isAnomaly ? (cfg.deviations ?? 3) : 3,
        direction: coerce(
          DIRECTION_VALUES,
          isAnomaly ? (cfg.direction ?? 'both') : 'both',
          'both',
        ),
        seasonality: coerce(
          SEASONALITY_VALUES,
          isAnomaly ? (cfg.seasonality ?? 'none') : 'none',
          'none',
        ),
        window_secs: existing.window_secs,
        for_duration_secs: existing.for_duration_secs,
        severity: coerce(SEVERITY_VALUES, existing.severity, 'warning'),
        enabled: existing.enabled,
        label_filters: tuplesToLabelFilters(existing.label_filters),
        group_by: existing.group_by,
        dynamic_alerts: existing.dynamic_alerts,
        max_series: existing.max_series,
        grouped_notification_threshold: existing.grouped_notification_threshold,
      }
    }
    return emptyDefaults()
  }, [existing])

  // `defaultValues` (mount-once), not `values`: this body is remounted via `key`
  // when the edited rule loads, so the form never resets a Select post-mount.
  const form = useForm<AlertFormData>({
    resolver: zodResolver(alertSchema),
    defaultValues,
  })
  // Drives which detector fields are shown. Hidden fields keep their values in
  // form state (RHF does not unregister), so `onSubmit` reads the right ones.
  const detectionKind = form.watch('detection_kind')
  const watchedMetric = form.watch('metric_name')
  const isAnomaly = detectionKind === 'anomaly'
  const groupBy = form.watch('group_by')
  const dynamicAlerts = form.watch('dynamic_alerts')
  const maxSeries = form.watch('max_series')
  const hasGroupBy = groupBy.length > 0
  const labelFilters = form.watch('label_filters')
  const alarmsBasePath = '/monitoring/alarms'

  // "Scope" (label filters + break-down/per-series settings) is the least
  // commonly needed section — most alerts just watch the whole metric — so it
  // starts collapsed. It starts OPEN when editing a rule that already has
  // something configured there, so nobody loses sight of their own settings.
  // Computed once at mount (not reactive) so opening/closing it afterward
  // isn't fought by a value the user is actively changing.
  const [scopeOpen, setScopeOpen] = useState(
    () => defaultValues.label_filters.length > 0 || defaultValues.group_by.length > 0,
  )
  const scopeSummary = useMemo(() => {
    const parts: string[] = []
    if (labelFilters.length > 0) {
      parts.push(`${labelFilters.length} filter${labelFilters.length === 1 ? '' : 's'}`)
    }
    if (hasGroupBy) parts.push(`by ${groupBy.join(', ')}`)
    if (dynamicAlerts) parts.push('per series')
    return parts.length > 0 ? parts.join(' · ') : 'Whole metric'
  }, [labelFilters, hasGroupBy, groupBy, dynamicAlerts])
  // Full per-series snapshot (firing AND ok) for a dynamic rule, decoded from
  // the persisted `series_states` column and keyed by the human-readable series
  // label. Firing series sort first, then by breach magnitude, so the most
  // urgent rows lead.
  const seriesEntries = useMemo(
    () =>
      Object.entries(existing?.series_states ?? {})
        .map(([label, s]) => ({ label, ...s }))
        .sort((a, b) => {
          const af = a.state === 'firing' ? 0 : 1
          const bf = b.state === 'firing' ? 0 : 1
          return af - bf || Math.abs(b.value) - Math.abs(a.value)
        }),
    [existing],
  )

  // Per-series alerting needs a group_by to split the metric into series. The
  // "Alert per series" toggle only renders while a group_by is set, so if the
  // user clears every break-down key, reset the now-meaningless flag rather than
  // carrying a stale `true` into submit. (Anomaly + dynamic is now supported, so
  // detector kind no longer gates this.)
  useEffect(() => {
    if (dynamicAlerts && !hasGroupBy) {
      form.setValue('dynamic_alerts', false)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hasGroupBy, dynamicAlerts])

  // Same mount-once-open logic as `scopeOpen`: don't hide a non-default
  // algorithm/seasonality the user already configured behind a collapsed
  // "Advanced" section when they open the form to edit this rule.
  const [anomalyAdvancedOpen, setAnomalyAdvancedOpen] = useState(
    () => defaultValues.algorithm !== 'robust' || defaultValues.seasonality !== 'none',
  )

  // History/eligibility for anomaly rules: a metric needs enough past data for a
  // trustworthy baseline, otherwise the rule sits at "unknown" and never alerts.
  // Bounds are memoised once so the query key is stable (no refetch loop).
  const historyRange = useMemo(() => {
    const now = Date.now()
    return {
      start: new Date(now - 90 * 86_400_000).toISOString(),
      end: new Date(now).toISOString(),
    }
  }, [])
  const historyQuery = useQuery({
    ...queryMetricsOptions({
      query: {
        project_id: project.id,
        metric_name: watchedMetric,
        aggregation: 'count',
        bucket_interval: '1d',
        start_time: historyRange.start,
        end_time: historyRange.end,
      },
    }),
    enabled: isAnomaly && !!watchedMetric,
  })
  const historyDays = useMemo(() => {
    const buckets = historyQuery.data?.data ?? []
    if (!buckets.length) return 0
    const earliest = Math.min(
      ...buckets.map((b) => new Date(b.bucket).getTime()),
    )
    return Math.max(0, Math.round((Date.now() - earliest) / 86_400_000))
  }, [historyQuery.data])
  const enoughHistory = historyDays >= ANOMALY_MIN_HISTORY_DAYS

  // Bounds for the label-filter autocomplete — mirrors LabelFilterBuilder's own
  // "last 24h" observed-attributes window. Computed once via the `useState`
  // lazy-initializer form (not `useMemo`) so reading the current time happens
  // during mount, not on every render pass, keeping the query key stable.
  const [labelFilterRange] = useState(() => {
    const now = Date.now()
    return {
      start: new Date(now - 24 * 60 * 60 * 1000).toISOString(),
      end: new Date(now).toISOString(),
    }
  })

  const createMutation = useMutation({
    ...createAlertMutation(),
    meta: { errorTitle: 'Failed to create alert rule' },
    onSuccess: () => {
      toast.success('Alert rule created')
      queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as Record<string, unknown>)?._id === 'listAlerts',
      })
      navigate('..')
    },
  })

  const updateMutation = useMutation({
    ...updateAlertMutation(),
    meta: { errorTitle: 'Failed to update alert rule' },
    onSuccess: () => {
      toast.success('Alert rule updated')
      queryClient.invalidateQueries({
        predicate: (query) => {
          const key = (query.queryKey[0] as Record<string, unknown>)?._id
          return key === 'listAlerts' || key === 'getAlert'
        },
      })
      navigate(-1)
    },
  })

  const isMutating = createMutation.isPending || updateMutation.isPending

  const onSubmit = async (data: AlertFormData) => {
    // Build the typed `detection_config` variant the user chose. The Zod enums
    // guarantee the string fields are valid literals, so the narrowing casts are
    // sound, and the backend re-validates the final config.
    const detection_config =
      data.detection_kind === 'anomaly'
        ? {
            kind: 'anomaly' as const,
            algorithm: data.algorithm as AnomalyAlgorithm,
            deviations: data.deviations,
            direction: data.direction as Direction,
            seasonality: data.seasonality as Seasonality,
          }
        : {
            kind: 'static' as const,
            comparator: data.comparator as Comparator,
            threshold: data.threshold,
          }
    const label_filters = labelFiltersToTuples(data.label_filters)
    // Defensive re-derivation, not just a UI nicety: dynamic_alerts only means
    // anything with a group_by set, so never submit it true otherwise even if a
    // stale toggle value slipped through. (Detector kind no longer matters — the
    // backend now supports per-series anomaly detection too.)
    const dynamic_alerts = data.dynamic_alerts && data.group_by.length > 0
    if (isEditing) {
      await updateMutation.mutateAsync({
        path: { id },
        query: { project_id: project.id },
        body: {
          name: data.name,
          metric_name: data.metric_name,
          aggregation: data.aggregation,
          detection_config,
          window_secs: data.window_secs,
          for_duration_secs: data.for_duration_secs,
          severity: data.severity,
          enabled: data.enabled,
          label_filters,
          group_by: data.group_by,
          dynamic_alerts,
          max_series: data.max_series,
          grouped_notification_threshold: data.grouped_notification_threshold,
        },
      })
    } else {
      await createMutation.mutateAsync({
        body: {
          project_id: project.id,
          name: data.name,
          metric_name: data.metric_name,
          aggregation: data.aggregation,
          detection_config,
          window_secs: data.window_secs,
          for_duration_secs: data.for_duration_secs,
          severity: data.severity,
          enabled: data.enabled,
          label_filters,
          group_by: data.group_by,
          dynamic_alerts,
          max_series: data.max_series,
          grouped_notification_threshold: data.grouped_notification_threshold,
        },
      })
    }
  }

  return (
    <div className="w-full space-y-6">
      <div className="flex items-center gap-4">
        <Button variant="ghost" size="icon" onClick={() => navigate(-1)}>
          <ArrowLeft className="size-4" />
        </Button>
        <div className="flex flex-1 flex-col gap-1">
          <div className="flex items-center gap-2">
            <h1 className="text-lg font-semibold">
              {isEditing ? 'Edit alert' : 'New alert'}
            </h1>
            {existing && (
              <AlertStateBadge
                state={existing.last_state}
                firingSeriesCount={
                  existing.dynamic_alerts
                    ? (existing.firing_series ?? []).length
                    : undefined
                }
              />
            )}
          </div>
          <p className="text-sm text-muted-foreground">
            {isEditing
              ? 'Update the signal, threshold, and notification settings.'
              : 'Fire a notification when a metric crosses a threshold.'}
          </p>
        </div>
      </div>

      {isEditing && existing?.dynamic_alerts && seriesEntries.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle className="text-base">
              Series ({seriesEntries.length})
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            {existing.last_dropped_series_count > 0 && (
              <div className="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-3 text-xs text-amber-700 dark:text-amber-400">
                <AlertTriangle className="mt-0.5 size-4 shrink-0" />
                <span>
                  {existing.last_dropped_series_count} series{' '}
                  {existing.last_dropped_series_count === 1 ? 'was' : 'were'}{' '}
                  dropped by the cardinality cap on the last evaluation — consider
                  raising <strong>Max series to track</strong>.
                </span>
              </div>
            )}
            {/* Every tracked series (firing AND ok), bounded by max_series
                server-side (100 max), so a plain scroll container is enough —
                no separate "N of M" cap note. */}
            <div className="max-h-72 overflow-y-auto rounded-md border border-border/60">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[90px]">Status</TableHead>
                    <TableHead>Series</TableHead>
                    <TableHead className="w-[100px] text-right">Value</TableHead>
                    <TableHead className="w-[90px] text-right">Alarm</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {seriesEntries.map((series, i) => {
                    const isFiring = series.state === 'firing'
                    return (
                      <TableRow key={`${series.label}-${i}`}>
                        <TableCell>
                          <span className="flex items-center gap-1.5 text-xs">
                            <span
                              className={cn(
                                'inline-block size-2 shrink-0 rounded-full',
                                isFiring ? 'bg-destructive' : 'bg-emerald-500',
                              )}
                            />
                            {isFiring ? 'Firing' : 'OK'}
                          </span>
                        </TableCell>
                        <TableCell className="font-mono text-xs">
                          {series.label || '(no labels)'}
                        </TableCell>
                        <TableCell className="text-right font-mono text-xs tabular-nums">
                          {formatMetricValue(series.value)}
                        </TableCell>
                        <TableCell className="text-right">
                          {isFiring && series.alarm_id != null ? (
                            <Link
                              to={`${alarmsBasePath}?project_id=${project.id}&alarm_id=${series.alarm_id}`}
                              className="text-xs underline underline-offset-2 hover:no-underline"
                            >
                              View
                            </Link>
                          ) : (
                            <span className="text-xs text-muted-foreground">
                              —
                            </span>
                          )}
                        </TableCell>
                      </TableRow>
                    )
                  })}
                </TableBody>
              </Table>
            </div>
          </CardContent>
        </Card>
      )}

      {isEditing &&
        (project.ai_debug_chat_enabled === true ||
          project.ai_write_actions_enabled === true) && (
        <DebugChat
          projectId={project.id}
          contextType="alert"
          contextId={id}
          title={existing ? `Investigate alert: ${existing.name}` : 'Investigate alert'}
          triggerLabel="Investigate with AI"
          description="Ask AI what this alert means, why it may be firing, and the prioritized steps to act on it."
          startPrompt="Explain what this alert means, why it may be firing, and the prioritized steps to investigate and resolve it."
          projectSlug={project.slug}
          projectName={project.name}
        />
      )}

      <Form {...form}>
        <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
          <Card>
            <CardHeader>
              <CardTitle>Basics</CardTitle>
            </CardHeader>
            <CardContent className="space-y-6">
              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="e.g. API p95 latency too high"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              {/* Signal: metric + aggregation */}
              <div className="grid gap-4 sm:grid-cols-2">
                <FormField
                  control={form.control}
                  name="metric_name"
                  render={({ field }) => (
                    <FormItem className="min-w-0">
                      <FormLabel>Metric</FormLabel>
                      <FormControl>
                        <SearchableSelect
                          value={field.value}
                          onValueChange={field.onChange}
                          options={metricOptions}
                          placeholder={
                            namesQuery.isPending
                              ? 'Loading metrics…'
                              : 'Select a metric…'
                          }
                          searchPlaceholder="Filter metrics…"
                          emptyText="No metrics ingested yet."
                          disabled={namesQuery.isPending}
                          className="font-mono text-xs"
                        />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="aggregation"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Aggregation</FormLabel>
                      <Select onValueChange={field.onChange} value={field.value}>
                        <FormControl>
                          <SelectTrigger className="font-mono text-xs">
                            <SelectValue />
                          </SelectTrigger>
                        </FormControl>
                        <SelectContent>
                          {AGGREGATIONS.map((a) => (
                            <SelectItem key={a.value} value={a.value}>
                              {a.label}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </div>
            </CardContent>
          </Card>

          {/* Scope: label filters + break-down/per-series settings. Collapsed
              by default — most alerts watch the whole metric and never touch
              this — but starts open when editing a rule that already has
              something configured here (see `scopeOpen`'s initializer). */}
          <Collapsible
            open={scopeOpen}
            onOpenChange={setScopeOpen}
            className="rounded-lg border border-border bg-card"
          >
            <CollapsibleTrigger
              type="button"
              className="flex w-full items-center justify-between gap-2 px-4 py-3 text-left text-sm transition-colors hover:bg-muted/40"
            >
              <span className="flex min-w-0 items-center gap-2">
                <SlidersHorizontal className="size-4 shrink-0 text-muted-foreground" />
                <span className="font-medium">Scope</span>
                <Badge variant="secondary" className="ml-1 shrink-0 font-normal">
                  {scopeSummary}
                </Badge>
              </span>
              <ChevronDown
                className={cn(
                  'size-4 shrink-0 text-muted-foreground transition-transform',
                  scopeOpen && 'rotate-180',
                )}
              />
            </CollapsibleTrigger>
            <CollapsibleContent className="flex flex-col gap-6 border-t border-border p-4">
              {/* Label filters — scopes this rule to one label value (e.g.
                  endpoint=/checkout) instead of the whole metric. Needs a
                  metric selected first: there's nothing to autocomplete
                  against otherwise, matching MetricsExplorer's gating of the
                  same autocomplete queries. */}
              {watchedMetric ? (
                <FormField
                  control={form.control}
                  name="label_filters"
                  render={({ field }) => (
                    <FormItem>
                      <FormControl>
                        <LabelFilterBuilder
                          value={field.value}
                          onChange={field.onChange}
                          projectId={project.id}
                          metricName={watchedMetric}
                          fromIso={labelFilterRange.start}
                          toIso={labelFilterRange.end}
                        />
                      </FormControl>
                      <FormDescription>
                        Scope this alert to a specific label value instead of
                        the whole metric. Leave empty to alert on the full
                        aggregate.
                      </FormDescription>
                    </FormItem>
                  )}
                />
              ) : (
                <p className="text-xs text-muted-foreground">
                  Pick a metric above to optionally scope this alert to a
                  specific label value.
                </p>
              )}

              {/* Group by / Alert per series (ADR-026 Phase 3/4) — pick up to 2
                  label keys to break the metric into per-series alarms instead
                  of one aggregate alarm. Same metric gating as label filters:
                  there's nothing to autocomplete without a metric. */}
              {watchedMetric && (
                <div className="flex flex-col gap-4 border-t border-border/60 pt-6">
                  <FormField
                    control={form.control}
                    name="group_by"
                    render={({ field }) => (
                      <FormItem>
                        <FormControl>
                          <GroupByBuilder
                            value={field.value}
                            onChange={field.onChange}
                            projectId={project.id}
                            metricName={watchedMetric}
                            fromIso={labelFilterRange.start}
                            toIso={labelFilterRange.end}
                            cardinalityHint={`only the top ${maxSeries} series are tracked once "Alert per series" is on; the rest are dropped for that evaluation tick.`}
                          />
                        </FormControl>
                        <FormDescription>
                          Break this alert down by label so it can fire
                          independently per series (e.g. one alarm per{' '}
                          <span className="font-mono">endpoint</span>) instead
                          of a single aggregate alarm.
                        </FormDescription>
                      </FormItem>
                    )}
                  />

                  {hasGroupBy && (
                    <FormField
                      control={form.control}
                      name="dynamic_alerts"
                      render={({ field }) => (
                        <FormItem className="flex flex-row items-center justify-between gap-4 rounded-md border border-border/60 p-3">
                          <div className="space-y-0.5">
                            <FormLabel>Alert per series</FormLabel>
                            <FormDescription>
                              Fire one independent alarm per breaching{' '}
                              {groupBy.join('/')} instead of a single alarm for
                              the whole metric.
                            </FormDescription>
                          </div>
                          <FormControl>
                            <Switch
                              checked={field.value}
                              onCheckedChange={field.onChange}
                            />
                          </FormControl>
                        </FormItem>
                      )}
                    />
                  )}

                  {hasGroupBy && dynamicAlerts && (
                    <div className="grid gap-4 sm:grid-cols-2">
                      <FormField
                        control={form.control}
                        name="max_series"
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Max series to track</FormLabel>
                            <FormControl>
                              <Input
                                type="number"
                                min={1}
                                max={100}
                                {...field}
                                onChange={(e) =>
                                  field.onChange(
                                    e.target.value === ''
                                      ? undefined
                                      : e.target.valueAsNumber,
                                  )
                                }
                              />
                            </FormControl>
                            <FormDescription>
                              Cardinality cap (1–100). Only the top series by
                              breach magnitude are tracked each tick; the rest
                              are dropped rather than creating unbounded alarms.
                            </FormDescription>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                      <FormField
                        control={form.control}
                        name="grouped_notification_threshold"
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Notification grouping threshold</FormLabel>
                            <FormControl>
                              <Input
                                type="number"
                                min={1}
                                max={1000}
                                {...field}
                                onChange={(e) =>
                                  field.onChange(
                                    e.target.value === ''
                                      ? undefined
                                      : e.target.valueAsNumber,
                                  )
                                }
                              />
                            </FormControl>
                            <FormDescription>
                              When more than this many series fire in the same
                              tick, you&apos;ll get one combined notification
                              instead of one per series.
                            </FormDescription>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                    </div>
                  )}
                </div>
              )}
            </CollapsibleContent>
          </Collapsible>

          <Card>
            <CardHeader>
              <CardTitle>Detection</CardTitle>
            </CardHeader>
            <CardContent className="space-y-6">
              {/* Detector type */}
              <FormField
                control={form.control}
                name="detection_kind"
                render={({ field }) => (
                  <FormItem className="sm:max-w-[280px]">
                    <FormLabel>Detection</FormLabel>
                    <Select onValueChange={field.onChange} value={field.value}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {DETECTION_KINDS.map((d) => (
                          <SelectItem key={d.value} value={d.value}>
                            {d.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormDescription>
                      {detectionKind === 'anomaly'
                        ? 'Learns a baseline band from history and fires when the metric deviates from it — no fixed number to pick.'
                        : 'Fires when the aggregated value crosses a fixed threshold.'}
                    </FormDescription>
                  </FormItem>
                )}
              />

              {detectionKind === 'anomaly' ? (
                <>
                  {/* History / eligibility — anomaly rules are silently inert
                      until the metric has enough past data to baseline. */}
                  {watchedMetric &&
                    !historyQuery.isPending &&
                    (enoughHistory ? (
                      <div className="flex items-start gap-2 rounded-md border border-border/60 bg-muted/30 p-3 text-xs text-muted-foreground">
                        <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-emerald-500" />
                        <span>
                          ~{historyDays} days of history available — enough to
                          baseline this metric.
                        </span>
                      </div>
                    ) : (
                      <div className="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-3 text-xs text-amber-700 dark:text-amber-400">
                        <AlertTriangle className="mt-0.5 size-4 shrink-0" />
                        <span>
                          Only ~{historyDays} day{historyDays === 1 ? '' : 's'}{' '}
                          of history for this metric. Anomaly detection needs
                          about {ANOMALY_MIN_HISTORY_DAYS} days to learn a
                          baseline — until then this rule stays{' '}
                          <strong>“unknown”</strong> and won&apos;t alert. You
                          can still save it; it starts working as history
                          accrues.
                        </span>
                      </div>
                    ))}

                  {/* Primary anomaly controls: sensitivity + direction. */}
                  <div className="grid gap-4 sm:grid-cols-2">
                    <FormField
                      control={form.control}
                      name="deviations"
                      render={({ field }) => {
                        const preset = presetForDeviations(field.value)
                        return (
                          <FormItem>
                            <FormLabel>Sensitivity</FormLabel>
                            <Select
                              value={preset}
                              onValueChange={(v) => {
                                const p = SENSITIVITY_PRESETS.find(
                                  (x) => x.value === v,
                                )
                                // 'custom' keeps the value + reveals the σ input.
                                if (p) field.onChange(p.deviations)
                              }}
                            >
                              <FormControl>
                                <SelectTrigger>
                                  <SelectValue />
                                </SelectTrigger>
                              </FormControl>
                              <SelectContent>
                                {SENSITIVITY_PRESETS.map((p) => (
                                  <SelectItem key={p.value} value={p.value}>
                                    {p.label}
                                  </SelectItem>
                                ))}
                                <SelectItem value="custom">Custom…</SelectItem>
                              </SelectContent>
                            </Select>
                            {preset === 'custom' && (
                              <FormControl>
                                <Input
                                  type="number"
                                  step="0.1"
                                  min={0}
                                  value={field.value}
                                  onChange={(e) =>
                                    field.onChange(
                                      e.target.value === ''
                                        ? undefined
                                        : e.target.valueAsNumber,
                                    )
                                  }
                                />
                              </FormControl>
                            )}
                            <FormDescription>
                              How far from normal counts as an anomaly. Higher =
                              more alerts.
                            </FormDescription>
                            <FormMessage />
                          </FormItem>
                        )
                      }}
                    />
                    <FormField
                      control={form.control}
                      name="direction"
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel>Direction</FormLabel>
                          <Select
                            onValueChange={field.onChange}
                            value={field.value}
                          >
                            <FormControl>
                              <SelectTrigger>
                                <SelectValue />
                              </SelectTrigger>
                            </FormControl>
                            <SelectContent>
                              {DIRECTIONS.map((d) => (
                                <SelectItem key={d.value} value={d.value}>
                                  {d.label}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                  </div>

                  {/* Advanced: algorithm + seasonality (sensible defaults). */}
                  <Collapsible
                    open={anomalyAdvancedOpen}
                    onOpenChange={setAnomalyAdvancedOpen}
                    className="rounded-md border border-border/60"
                  >
                    <CollapsibleTrigger
                      type="button"
                      className="flex w-full items-center justify-between gap-2 px-3 py-2 text-left text-sm font-medium text-muted-foreground transition-colors hover:bg-muted/40"
                    >
                      Advanced
                      <ChevronDown
                        className={cn(
                          'size-4 shrink-0 transition-transform',
                          anomalyAdvancedOpen && 'rotate-180',
                        )}
                      />
                    </CollapsibleTrigger>
                    <CollapsibleContent className="border-t border-border/60 p-3">
                      <div className="grid gap-4 sm:grid-cols-2">
                        <FormField
                          control={form.control}
                          name="algorithm"
                          render={({ field }) => (
                            <FormItem>
                              <FormLabel>Algorithm</FormLabel>
                              <Select
                                onValueChange={field.onChange}
                                value={field.value}
                              >
                                <FormControl>
                                  <SelectTrigger>
                                    <SelectValue />
                                  </SelectTrigger>
                                </FormControl>
                                <SelectContent>
                                  {ANOMALY_ALGORITHMS.map((a) => (
                                    <SelectItem key={a.value} value={a.value}>
                                      {a.label}
                                    </SelectItem>
                                  ))}
                                </SelectContent>
                              </Select>
                              <FormMessage />
                            </FormItem>
                          )}
                        />
                        <FormField
                          control={form.control}
                          name="seasonality"
                          render={({ field }) => (
                            <FormItem>
                              <FormLabel>Seasonality</FormLabel>
                              <Select
                                onValueChange={field.onChange}
                                value={field.value}
                              >
                                <FormControl>
                                  <SelectTrigger>
                                    <SelectValue />
                                  </SelectTrigger>
                                </FormControl>
                                <SelectContent>
                                  {SEASONALITIES.map((s) => (
                                    <SelectItem key={s.value} value={s.value}>
                                      {s.label}
                                    </SelectItem>
                                  ))}
                                </SelectContent>
                              </Select>
                              <FormDescription>
                                Compare like-for-like times (e.g. weekly = same
                                weekday &amp; hour). Needs more history.
                              </FormDescription>
                              <FormMessage />
                            </FormItem>
                          )}
                        />
                      </div>
                    </CollapsibleContent>
                  </Collapsible>

                  {/* "Would this have fired?" — only meaningful with a metric. */}
                  {watchedMetric && (
                    <AnomalyBacktest
                      projectId={project.id}
                      metricName={watchedMetric}
                      aggregation={form.watch('aggregation')}
                      windowSecs={form.watch('window_secs')}
                      detectionConfig={{
                        kind: 'anomaly',
                        algorithm: form.watch('algorithm') as AnomalyAlgorithm,
                        deviations: form.watch('deviations'),
                        direction: form.watch('direction') as Direction,
                        seasonality: form.watch('seasonality') as Seasonality,
                      }}
                    />
                  )}
                </>
              ) : (
                /* Static: comparator + threshold */
                <div className="grid gap-4 sm:grid-cols-2">
                  <FormField
                    control={form.control}
                    name="comparator"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Comparator</FormLabel>
                        <Select
                          onValueChange={field.onChange}
                          value={field.value}
                        >
                          <FormControl>
                            <SelectTrigger>
                              <SelectValue />
                            </SelectTrigger>
                          </FormControl>
                          <SelectContent>
                            {COMPARATORS.map((c) => (
                              <SelectItem key={c.value} value={c.value}>
                                {c.label}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <FormField
                    control={form.control}
                    name="threshold"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Threshold</FormLabel>
                        <FormControl>
                          <Input
                            type="number"
                            step="any"
                            {...field}
                            onChange={(e) =>
                              field.onChange(
                                e.target.value === ''
                                  ? undefined
                                  : e.target.valueAsNumber,
                              )
                            }
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </div>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Timing &amp; notifications</CardTitle>
            </CardHeader>
            <CardContent className="space-y-6">
              {/* Timing: window + for-duration */}
              <div className="grid gap-4 sm:grid-cols-2">
                <FormField
                  control={form.control}
                  name="window_secs"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Evaluation window (seconds)</FormLabel>
                      <FormControl>
                        <Input
                          type="number"
                          min={1}
                          {...field}
                          onChange={(e) =>
                            field.onChange(
                              e.target.value === ''
                                ? undefined
                                : e.target.valueAsNumber,
                            )
                          }
                        />
                      </FormControl>
                      <FormDescription>
                        The metric is aggregated over this trailing window each
                        evaluation (e.g. 300 = last 5 minutes).
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="for_duration_secs"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>For duration (seconds)</FormLabel>
                      <FormControl>
                        <Input
                          type="number"
                          min={1}
                          {...field}
                          onChange={(e) =>
                            field.onChange(
                              e.target.value === ''
                                ? undefined
                                : e.target.valueAsNumber,
                            )
                          }
                        />
                      </FormControl>
                      <FormDescription>
                        The breach must persist this long before the alert fires,
                        to avoid flapping.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </div>

              {/* Severity */}
              <FormField
                control={form.control}
                name="severity"
                render={({ field }) => (
                  <FormItem className="sm:max-w-[240px]">
                    <FormLabel>Severity</FormLabel>
                    <Select onValueChange={field.onChange} value={field.value}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {SEVERITIES.map((s) => (
                          <SelectItem key={s.value} value={s.value}>
                            {s.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormDescription>
                      Maps to the notification severity. Alerts are delivered
                      through the notification channels configured for this
                      project.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              {/* Enabled toggle */}
              <FormField
                control={form.control}
                name="enabled"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-center justify-between rounded-md border border-border/60 p-3">
                    <div className="space-y-0.5">
                      <FormLabel>Enabled</FormLabel>
                      <FormDescription>
                        Only enabled rules are evaluated by the background
                        evaluator.
                      </FormDescription>
                    </div>
                    <FormControl>
                      <Switch
                        checked={field.value}
                        onCheckedChange={field.onChange}
                      />
                    </FormControl>
                  </FormItem>
                )}
              />
            </CardContent>
          </Card>

          <div className="flex items-center gap-3 border-t pt-4">
            <Button type="submit" disabled={isMutating}>
              {isMutating
                ? 'Saving…'
                : isEditing
                  ? 'Save changes'
                  : 'Create alert'}
            </Button>
            <Button type="button" variant="outline" onClick={() => navigate(-1)}>
              Cancel
            </Button>
          </div>
        </form>
      </Form>
    </div>
  )
}

/**
 * Loads the edited rule (if any), then mounts {@link AlertFormBody} with a `key`
 * so the form is constructed once with the resolved values — never initialized
 * empty and reset, which would drop changed Radix `Select` values.
 */
export default function MetricAlertForm({ project }: MetricAlertFormProps) {
  const { alertId } = useParams()
  const isEditing = !!alertId
  const id = Number(alertId)

  const existingQuery = useQuery({
    ...getAlertOptions({ path: { id }, query: { project_id: project.id } }),
    enabled: isEditing && Number.isFinite(id),
  })

  if (isEditing && existingQuery.isPending) {
    return (
      <div className="w-full space-y-6">
        <div className="flex items-center gap-4">
          <Skeleton className="size-9" />
          <Skeleton className="h-8 w-48" />
        </div>
        <Skeleton className="h-[500px] w-full" />
      </div>
    )
  }

  return (
    <AlertFormBody
      key={isEditing ? `edit-${id}` : 'new'}
      project={project}
      isEditing={isEditing}
      id={id}
      existing={existingQuery.data ?? null}
    />
  )
}
