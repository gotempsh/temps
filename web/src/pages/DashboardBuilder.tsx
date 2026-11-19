// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageTitle } from '@temps-sdk/ds'
import { ProjectResponse } from '@/api/client'
// REGEN: bun run openapi-ts — these builders/types come from the new
// /otel/dashboards endpoints (operationIds get_dashboard / create_dashboard /
// update_dashboard). They are absent from the committed SDK; imported here so
// the builder is regen-ready instead of hand-rolling fetch (project rule).
import {
  createDashboardMutation,
  getDashboardOptions,
  listMetricNamesOptions,
  listSavedSearchesOptions,
  updateDashboardMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Form,
  FormControl,
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
import { AGGREGATIONS } from '@/components/metrics/metric-format'
import {
  LOG_TILE_FUNCTIONS,
  formatLogTileMetric,
  isLogAttributeKey,
  logTileProblem,
  parseLogTileMetric,
  searchFiltersText,
  type LogTileFunction,
} from '@/components/metrics/log-tile'
import {
  GroupByBuilder,
  LabelFilterBuilder,
} from '@/components/metrics/LabelFilterBuilder'
import {
  labelFiltersToTuples,
  tuplesToLabelFilters,
} from '@/components/metrics/label-filters'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, GripVertical, Plus, Trash2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useFieldArray, useForm, useWatch, type Control } from 'react-hook-form'
import { Link, useNavigate, useParams } from 'react-router'
import { useGoBack } from '@/hooks/useGoBack'
import { toast } from 'sonner'
import { z } from 'zod'

interface DashboardBuilderProps {
  project: ProjectResponse
}

const AGGREGATION_VALUES = AGGREGATIONS.map((a) => a.value) as [
  string,
  ...string[],
]

const LOG_FUNCTION_VALUES = LOG_TILE_FUNCTIONS.map((f) => f.value) as [
  LogTileFunction,
  ...LogTileFunction[],
]

const tileSchema = z
  .object({
    id: z.string(),
    /** What the tile charts: a metric, or a saved log search over time. */
    kind: z.enum(['metric', 'logs']),
    metric_name: z.string(),
    aggregation: z.enum(AGGREGATION_VALUES),
    saved_search_id: z.number().nullable(),
    log_function: z.enum(LOG_FUNCTION_VALUES),
    log_attribute: z.string(),
    title: z.string().optional(),
    label_filters: z.array(z.object({ key: z.string(), value: z.string() })),
    group_by: z.array(z.string()).max(2),
  })
  .superRefine((tile, ctx) => {
    if (tile.kind === 'metric' && !tile.metric_name)
      ctx.addIssue({
        code: 'custom',
        path: ['metric_name'],
        message: 'Pick a metric',
      })
    if (tile.kind === 'logs' && tile.saved_search_id == null)
      ctx.addIssue({
        code: 'custom',
        path: ['saved_search_id'],
        message: 'Pick a saved search',
      })
    if (
      tile.kind === 'logs' &&
      tile.log_function !== 'count' &&
      !isLogAttributeKey(tile.log_attribute)
    )
      ctx.addIssue({
        code: 'custom',
        path: ['log_attribute'],
        message:
          'Name the attribute to chart, e.g. duration_ms: letters, digits, _ and ., starting with a letter or _',
      })
  })

const sectionSchema = z.object({
  id: z.string(),
  title: z.string().min(1, 'Section title is required'),
  tiles: z.array(tileSchema),
})

const dashboardSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  sections: z.array(sectionSchema),
})

type DashboardFormData = z.infer<typeof dashboardSchema>

/** The project's saved log searches, for a tile that charts one. */
interface SavedSearchChoices {
  options: { value: string; label: string }[]
  /** Ids of the searches that filter message text. */
  textFiltered: Set<number>
  loading: boolean
  /** Where to save a first search when there is none. */
  logsHref: string
}

/** Stable-enough client id for new sections/tiles (UI key + layout id). */
function makeId(prefix: string): string {
  return `${prefix}-${Math.random().toString(36).slice(2, 10)}`
}

/**
 * Narrow a stored aggregation string (the backend field is a plain `string`)
 * into the form's enum union, falling back to `avg` for unknown values so the
 * select stays controlled.
 */
function coerceAggregation(value: string): (typeof AGGREGATION_VALUES)[number] {
  return (AGGREGATION_VALUES as readonly string[]).includes(value)
    ? (value as (typeof AGGREGATION_VALUES)[number])
    : 'avg'
}

function emptyDefaults(): DashboardFormData {
  return {
    name: '',
    sections: [
      {
        id: makeId('section'),
        title: 'Overview',
        tiles: [],
      },
    ],
  }
}

export default function DashboardBuilder({ project }: DashboardBuilderProps) {
  const navigate = useNavigate()
  const goBack = useGoBack(`/projects/${project.slug}/metrics/dashboards`)
  const queryClient = useQueryClient()
  const { dashboardId } = useParams()
  const isEditing = !!dashboardId
  const id = Number(dashboardId)

  const existingQuery = useQuery({
    ...getDashboardOptions({ path: { id }, query: { project_id: project.id } }),
    enabled: isEditing && Number.isFinite(id),
  })

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
    [namesQuery.data]
  )
  const savedSearchesQuery = useQuery({
    ...listSavedSearchesOptions({
      path: { project_id: project.id },
      query: { page_size: 100 },
    }),
    enabled: !!project.id,
  })
  const savedSearches = useMemo<SavedSearchChoices>(
    () => ({
      options: (savedSearchesQuery.data?.data ?? []).map((search) => ({
        value: String(search.id),
        label: search.name,
      })),
      textFiltered: new Set(
        (savedSearchesQuery.data?.data ?? [])
          .filter((search) => searchFiltersText(search.query))
          .map((search) => search.id)
      ),
      loading: savedSearchesQuery.isPending,
      logsHref: `/logs?project_id=${project.id}`,
    }),
    [savedSearchesQuery.data, savedSearchesQuery.isPending, project.id]
  )

  // Bounds for the label-filter autocomplete — mirrors LabelFilterBuilder's own
  // "last 24h" observed-attributes window. Computed once via the `useState`
  // lazy-initializer form (not `useMemo`) so reading the current time happens
  // during mount, not on every render pass, keeping the query key stable.
  const [labelFilterRange] = useState(() => {
    const now = Date.now()
    return {
      from: new Date(now - 24 * 60 * 60 * 1000).toISOString(),
      to: new Date(now).toISOString(),
    }
  })

  const defaultValues = useMemo<DashboardFormData>(() => {
    const existing = existingQuery.data
    if (existing) {
      const sections = existing.layout?.sections ?? []
      return {
        name: existing.name,
        sections:
          sections.length > 0
            ? sections.map((s) => ({
                id: s.id || makeId('section'),
                title: s.title,
                tiles: (s.tiles ?? []).map((t) => ({
                  id: t.id || makeId('tile'),
                  kind:
                    t.source?.kind === 'logs'
                      ? ('logs' as const)
                      : ('metric' as const),
                  metric_name: t.metric_name,
                  aggregation: coerceAggregation(t.aggregation),
                  saved_search_id:
                    t.source?.kind === 'logs' ? t.source.saved_search_id : null,
                  log_function: parseLogTileMetric(
                    t.source?.kind === 'logs' ? t.source.metric : undefined
                  ).fn,
                  log_attribute: parseLogTileMetric(
                    t.source?.kind === 'logs' ? t.source.metric : undefined
                  ).attribute,
                  title: t.title ?? '',
                  label_filters: tuplesToLabelFilters(t.label_filters),
                  group_by: t.group_by ?? [],
                })),
              }))
            : emptyDefaults().sections,
      }
    }
    return emptyDefaults()
  }, [existingQuery.data])

  const form = useForm<DashboardFormData>({
    resolver: zodResolver(dashboardSchema),
    values: defaultValues,
  })

  const sectionsArray = useFieldArray({
    control: form.control,
    name: 'sections',
  })

  const createMutation = useMutation({
    ...createDashboardMutation(),
    meta: { errorTitle: 'Failed to create dashboard' },
    onSuccess: () => {
      toast.success('Dashboard created')
      queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as Record<string, unknown>)?._id ===
          'listDashboards',
      })
      navigate('..')
    },
  })

  const updateMutation = useMutation({
    ...updateDashboardMutation(),
    meta: { errorTitle: 'Failed to update dashboard' },
    onSuccess: () => {
      toast.success('Dashboard updated')
      queryClient.invalidateQueries({
        predicate: (query) => {
          const key = (query.queryKey[0] as Record<string, unknown>)?._id
          return key === 'listDashboards' || key === 'getDashboard'
        },
      })
      goBack()
    },
  })

  const isMutating = createMutation.isPending || updateMutation.isPending

  const onSubmit = async (data: DashboardFormData) => {
    // A numeric field over a text-filtered search would fail every time the
    // tile loads; refuse it here, on the field that has to change.
    let refused = false
    data.sections.forEach((section, sectionIndex) =>
      section.tiles.forEach((tile, tileIndex) => {
        if (tile.kind !== 'logs' || tile.saved_search_id == null) return
        const problem = logTileProblem(
          tile.log_function,
          savedSearches.textFiltered.has(tile.saved_search_id)
        )
        if (problem === null) return
        refused = true
        form.setError(
          `sections.${sectionIndex}.tiles.${tileIndex}.log_function`,
          { type: 'custom', message: problem }
        )
      })
    )
    if (refused) return
    const layout = {
      sections: data.sections.map((s) => ({
        id: s.id,
        title: s.title,
        tiles: s.tiles.map((t) =>
          t.kind === 'logs' && t.saved_search_id != null
            ? {
                id: t.id,
                // A log tile charts its saved search; the metric fields are
                // kept neutral so the row stays a valid tile.
                metric_name: '',
                aggregation: 'count',
                title: t.title?.trim() ? t.title.trim() : null,
                label_filters: [],
                group_by: [],
                source: {
                  kind: 'logs' as const,
                  saved_search_id: t.saved_search_id,
                  metric: formatLogTileMetric(t.log_function, t.log_attribute),
                },
              }
            : {
                id: t.id,
                metric_name: t.metric_name,
                aggregation: t.aggregation,
                title: t.title?.trim() ? t.title.trim() : null,
                label_filters: labelFiltersToTuples(t.label_filters),
                group_by: t.group_by,
                source: { kind: 'metric' as const },
              }
        ),
      })),
    }

    if (isEditing) {
      await updateMutation.mutateAsync({
        path: { id },
        query: { project_id: project.id },
        body: { name: data.name, layout },
      })
    } else {
      await createMutation.mutateAsync({
        body: { project_id: project.id, name: data.name, layout },
      })
    }
  }

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
    <div className="w-full space-y-6">
      <div className="flex items-center gap-4">
        <Button
          aria-label="Back"
          variant="ghost"
          size="icon"
          onClick={() => goBack()}
        >
          <ArrowLeft className="size-4" />
        </Button>
        <div>
          <PageTitle className="text-lg font-semibold">
            {isEditing ? 'Edit dashboard' : 'New dashboard'}
          </PageTitle>
          <p className="text-sm text-muted-foreground">
            {isEditing
              ? 'Update sections and metric tiles.'
              : 'Group OpenTelemetry metrics into sections of charts.'}
          </p>
        </div>
      </div>

      <Form {...form}>
        <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
          <Card>
            <CardHeader>
              <CardTitle>Dashboard</CardTitle>
            </CardHeader>
            <CardContent>
              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input placeholder="e.g. API latency" {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </CardContent>
          </Card>

          <div className="flex flex-col gap-4">
            {sectionsArray.fields.map((sectionField, sectionIndex) => (
              <SectionEditor
                key={sectionField.id}
                control={form.control}
                sectionIndex={sectionIndex}
                metricOptions={metricOptions}
                metricsLoading={namesQuery.isPending}
                savedSearches={savedSearches}
                projectId={project.id}
                labelFilterFromIso={labelFilterRange.from}
                labelFilterToIso={labelFilterRange.to}
                onRemoveSection={() => sectionsArray.remove(sectionIndex)}
                canRemoveSection={sectionsArray.fields.length > 1}
              />
            ))}
          </div>

          <Button
            type="button"
            variant="outline"
            onClick={() =>
              sectionsArray.append({
                id: makeId('section'),
                title: '',
                tiles: [],
              })
            }
            className="gap-1.5"
          >
            <Plus className="size-4" />
            Add section
          </Button>

          <div className="flex items-center gap-3 border-t pt-4">
            <Button type="submit" disabled={isMutating}>
              {isMutating
                ? 'Saving…'
                : isEditing
                  ? 'Save changes'
                  : 'Create dashboard'}
            </Button>
            <Button type="button" variant="outline" onClick={() => goBack()}>
              Cancel
            </Button>
          </div>
        </form>
      </Form>
    </div>
  )
}

function SectionEditor({
  control,
  sectionIndex,
  metricOptions,
  metricsLoading,
  savedSearches,
  projectId,
  labelFilterFromIso,
  labelFilterToIso,
  onRemoveSection,
  canRemoveSection,
}: {
  control: Control<DashboardFormData>
  sectionIndex: number
  metricOptions: { value: string; label: string }[]
  metricsLoading: boolean
  savedSearches: SavedSearchChoices
  projectId: number
  labelFilterFromIso: string
  labelFilterToIso: string
  onRemoveSection: () => void
  canRemoveSection: boolean
}) {
  const tilesArray = useFieldArray({
    control,
    name: `sections.${sectionIndex}.tiles`,
  })

  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-3 space-y-0">
        <div className="flex flex-1 items-center gap-2">
          <GripVertical className="size-4 shrink-0 text-muted-foreground" />
          <FormField
            control={control}
            name={`sections.${sectionIndex}.title`}
            render={({ field }) => (
              <FormItem className="flex-1">
                <FormControl>
                  <Input
                    placeholder="Section title"
                    className="font-medium"
                    {...field}
                  />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="size-8 shrink-0 text-muted-foreground"
          onClick={onRemoveSection}
          disabled={!canRemoveSection}
          aria-label="Remove section"
        >
          <Trash2 className="size-4" />
        </Button>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {tilesArray.fields.length === 0 ? (
          <p className="text-xs text-muted-foreground">
            No tiles yet. Add a metric tile to this section.
          </p>
        ) : (
          tilesArray.fields.map((tileField, tileIndex) => (
            <TileEditor
              key={tileField.id}
              control={control}
              sectionIndex={sectionIndex}
              tileIndex={tileIndex}
              metricOptions={metricOptions}
              metricsLoading={metricsLoading}
              savedSearches={savedSearches}
              projectId={projectId}
              labelFilterFromIso={labelFilterFromIso}
              labelFilterToIso={labelFilterToIso}
              onRemove={() => tilesArray.remove(tileIndex)}
            />
          ))
        )}
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="gap-1.5 self-start text-xs"
          onClick={() =>
            tilesArray.append({
              id: makeId('tile'),
              kind: 'metric',
              metric_name: '',
              aggregation: 'avg',
              saved_search_id: null,
              log_function: 'count',
              log_attribute: '',
              title: '',
              label_filters: [],
              group_by: [],
            })
          }
        >
          <Plus className="size-3.5" />
          Add tile
        </Button>
      </CardContent>
    </Card>
  )
}

function TileEditor({
  control,
  sectionIndex,
  tileIndex,
  metricOptions,
  metricsLoading,
  savedSearches,
  projectId,
  labelFilterFromIso,
  labelFilterToIso,
  onRemove,
}: {
  control: Control<DashboardFormData>
  sectionIndex: number
  tileIndex: number
  metricOptions: { value: string; label: string }[]
  metricsLoading: boolean
  savedSearches: SavedSearchChoices
  projectId: number
  labelFilterFromIso: string
  labelFilterToIso: string
  onRemove: () => void
}) {
  // Watched independently of the field-array snapshot so the label-filter
  // section below reacts as soon as a metric is picked on THIS row.
  const metricName = useWatch({
    control,
    name: `sections.${sectionIndex}.tiles.${tileIndex}.metric_name`,
  })
  const kind = useWatch({
    control,
    name: `sections.${sectionIndex}.tiles.${tileIndex}.kind`,
  })
  const logFunction = useWatch({
    control,
    name: `sections.${sectionIndex}.tiles.${tileIndex}.log_function`,
  })
  const savedSearchId = useWatch({
    control,
    name: `sections.${sectionIndex}.tiles.${tileIndex}.saved_search_id`,
  })
  const tilePath = `sections.${sectionIndex}.tiles.${tileIndex}` as const

  return (
    <div className="flex flex-col gap-2 rounded-md border border-border/60 p-3">
      <FormField
        control={control}
        name={`${tilePath}.kind`}
        render={({ field }) => (
          <FormItem className="sm:w-[240px]">
            <FormLabel className="text-xs">Chart</FormLabel>
            <Select onValueChange={field.onChange} value={field.value}>
              <FormControl>
                <SelectTrigger className="text-xs">
                  <SelectValue />
                </SelectTrigger>
              </FormControl>
              <SelectContent>
                <SelectItem value="metric">A metric</SelectItem>
                <SelectItem value="logs">A saved log search</SelectItem>
              </SelectContent>
            </Select>
          </FormItem>
        )}
      />
      {kind === 'logs' ? (
        <LogTileFields
          control={control}
          tilePath={tilePath}
          logFunction={logFunction}
          problem={
            savedSearchId == null
              ? null
              : logTileProblem(
                  logFunction,
                  savedSearches.textFiltered.has(savedSearchId)
                )
          }
          savedSearches={savedSearches}
          onRemove={onRemove}
        />
      ) : (
        <>
          <div className="flex flex-col gap-2 sm:flex-row sm:items-start">
            <FormField
              control={control}
              name={`sections.${sectionIndex}.tiles.${tileIndex}.metric_name`}
              render={({ field }) => (
                <FormItem className="min-w-0 flex-1">
                  <FormLabel className="text-xs">Metric</FormLabel>
                  <FormControl>
                    <SearchableSelect
                      value={field.value}
                      onValueChange={field.onChange}
                      options={metricOptions}
                      placeholder={
                        metricsLoading ? 'Loading metrics…' : 'Select a metric…'
                      }
                      searchPlaceholder="Filter metrics…"
                      emptyText="No metrics ingested yet."
                      disabled={metricsLoading}
                      className="font-mono text-xs"
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={control}
              name={`sections.${sectionIndex}.tiles.${tileIndex}.aggregation`}
              render={({ field }) => (
                <FormItem className="sm:w-[140px]">
                  <FormLabel className="text-xs">Aggregation</FormLabel>
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
            <FormField
              control={control}
              name={`sections.${sectionIndex}.tiles.${tileIndex}.title`}
              render={({ field }) => (
                <FormItem className="min-w-0 flex-1">
                  <FormLabel className="text-xs">Title (optional)</FormLabel>
                  <FormControl>
                    <Input
                      placeholder="Override tile title"
                      className="text-xs"
                      {...field}
                      value={field.value ?? ''}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="size-8 shrink-0 self-end text-muted-foreground sm:mt-6"
              onClick={onRemove}
              aria-label="Remove tile"
            >
              <Trash2 className="size-4" />
            </Button>
          </div>

          {/* Label filters — scopes this tile's chart to one label value. Needs a
          metric selected first, matching the alert form's gating. */}
          {metricName ? (
            <>
              <FormField
                control={control}
                name={`sections.${sectionIndex}.tiles.${tileIndex}.label_filters`}
                render={({ field }) => (
                  <FormItem>
                    <FormControl>
                      <LabelFilterBuilder
                        value={field.value}
                        onChange={field.onChange}
                        projectId={projectId}
                        metricName={metricName}
                        fromIso={labelFilterFromIso}
                        toIso={labelFilterToIso}
                      />
                    </FormControl>
                  </FormItem>
                )}
              />
              <FormField
                control={control}
                name={`sections.${sectionIndex}.tiles.${tileIndex}.group_by`}
                render={({ field }) => (
                  <FormItem>
                    <FormControl>
                      <GroupByBuilder
                        value={field.value}
                        onChange={field.onChange}
                        projectId={projectId}
                        metricName={metricName}
                        fromIso={labelFilterFromIso}
                        toIso={labelFilterToIso}
                      />
                    </FormControl>
                  </FormItem>
                )}
              />
            </>
          ) : (
            <p className="text-xs text-muted-foreground">
              Pick a metric to optionally scope this tile by label.
            </p>
          )}
        </>
      )}
    </div>
  )
}

/** The fields of a tile that charts a saved log search. */
function LogTileFields({
  control,
  tilePath,
  logFunction,
  problem,
  savedSearches,
  onRemove,
}: {
  control: Control<DashboardFormData>
  tilePath: `sections.${number}.tiles.${number}`
  logFunction: LogTileFunction
  /** Why this combination cannot be charted, shown under the Value field. */
  problem: string | null
  savedSearches: SavedSearchChoices
  onRemove: () => void
}) {
  return (
    <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-start">
      <FormField
        control={control}
        name={`${tilePath}.saved_search_id`}
        render={({ field }) => (
          <FormItem className="min-w-0 flex-1">
            <FormLabel className="text-xs">Saved search</FormLabel>
            <FormControl>
              <SearchableSelect
                value={field.value != null ? String(field.value) : ''}
                onValueChange={(value) =>
                  field.onChange(value ? Number(value) : null)
                }
                options={savedSearches.options}
                placeholder={
                  savedSearches.loading
                    ? 'Loading saved searches…'
                    : 'Select a saved search…'
                }
                searchPlaceholder="Filter saved searches…"
                emptyText="No saved searches in this service yet."
                disabled={savedSearches.loading}
                className="text-xs"
              />
            </FormControl>
            {!savedSearches.loading && savedSearches.options.length === 0 && (
              <p className="text-xs text-muted-foreground">
                Save a search on the{' '}
                <Link
                  to={savedSearches.logsHref}
                  className="underline underline-offset-2"
                >
                  logs page
                </Link>{' '}
                first, then chart it here.
              </p>
            )}
            <FormMessage />
          </FormItem>
        )}
      />
      <FormField
        control={control}
        name={`${tilePath}.log_function`}
        render={({ field }) => (
          <FormItem className="sm:w-[170px]">
            <FormLabel className="text-xs">Value</FormLabel>
            <Select onValueChange={field.onChange} value={field.value}>
              <FormControl>
                <SelectTrigger className="text-xs">
                  <SelectValue />
                </SelectTrigger>
              </FormControl>
              <SelectContent>
                {LOG_TILE_FUNCTIONS.map((f) => (
                  <SelectItem key={f.value} value={f.value}>
                    {f.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {problem ? (
              <p className="text-xs text-destructive">{problem}</p>
            ) : (
              <FormMessage />
            )}
          </FormItem>
        )}
      />
      {logFunction !== 'count' && (
        <FormField
          control={control}
          name={`${tilePath}.log_attribute`}
          render={({ field }) => (
            <FormItem className="min-w-0 sm:w-[180px]">
              <FormLabel className="text-xs">Attribute</FormLabel>
              <FormControl>
                <Input
                  placeholder="duration_ms"
                  className="font-mono text-xs"
                  {...field}
                />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />
      )}
      <FormField
        control={control}
        name={`${tilePath}.title`}
        render={({ field }) => (
          <FormItem className="min-w-0 flex-1">
            <FormLabel className="text-xs">Title (optional)</FormLabel>
            <FormControl>
              <Input
                placeholder="Defaults to the search's name"
                className="text-xs"
                {...field}
                value={field.value ?? ''}
              />
            </FormControl>
            <FormMessage />
          </FormItem>
        )}
      />
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="size-8 shrink-0 self-end text-muted-foreground sm:mt-6"
        onClick={onRemove}
        aria-label="Remove tile"
      >
        <Trash2 className="size-4" />
      </Button>
    </div>
  )
}
