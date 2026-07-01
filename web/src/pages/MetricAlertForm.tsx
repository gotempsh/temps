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
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import { DebugChat } from '@/components/ai/DebugChat'
import { AGGREGATIONS } from '@/components/metrics/metric-format'
import { AnomalyBacktest } from '@/components/metrics/AnomalyBacktest'
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
import { AlertTriangle, ArrowLeft, CheckCircle2 } from 'lucide-react'
import { useMemo } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate, useParams } from 'react-router-dom'
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
            {existing && <AlertStateBadge state={existing.last_state} />}
          </div>
          <p className="text-sm text-muted-foreground">
            {isEditing
              ? 'Update the signal, threshold, and notification settings.'
              : 'Fire a notification when a metric crosses a threshold.'}
          </p>
        </div>
      </div>

      {isEditing && project.ai_debug_chat_enabled === true && (
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
              <CardTitle>Rule</CardTitle>
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
                  <details className="rounded-md border border-border/60 px-3 py-2 [&_summary]:cursor-pointer">
                    <summary className="text-sm font-medium text-muted-foreground">
                      Advanced
                    </summary>
                    <div className="mt-3 grid gap-4 sm:grid-cols-2">
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
                  </details>

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
                      through this project's configured notification channels.
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
