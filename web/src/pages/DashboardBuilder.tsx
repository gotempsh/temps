import { ProjectResponse } from '@/api/client'
// REGEN: bun run openapi-ts — these builders/types come from the new
// /otel/dashboards endpoints (operationIds get_dashboard / create_dashboard /
// update_dashboard). They are absent from the committed SDK; imported here so
// the builder is regen-ready instead of hand-rolling fetch (project rule).
import {
  createDashboardMutation,
  getDashboardOptions,
  listMetricNamesOptions,
  updateDashboardMutation,
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
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, GripVertical, Plus, Trash2 } from 'lucide-react'
import { useMemo } from 'react'
import {
  useFieldArray,
  useForm,
  type Control,
} from 'react-hook-form'
import { useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'

interface DashboardBuilderProps {
  project: ProjectResponse
}

const AGGREGATION_VALUES = AGGREGATIONS.map((a) => a.value) as [
  string,
  ...string[],
]

const tileSchema = z.object({
  id: z.string(),
  metric_name: z.string().min(1, 'Pick a metric'),
  aggregation: z.enum(AGGREGATION_VALUES),
  title: z.string().optional(),
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
    [namesQuery.data],
  )

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
                  metric_name: t.metric_name,
                  aggregation: coerceAggregation(t.aggregation),
                  title: t.title ?? '',
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
      navigate(-1)
    },
  })

  const isMutating = createMutation.isPending || updateMutation.isPending

  const onSubmit = async (data: DashboardFormData) => {
    const layout = {
      sections: data.sections.map((s) => ({
        id: s.id,
        title: s.title,
        tiles: s.tiles.map((t) => ({
          id: t.id,
          metric_name: t.metric_name,
          aggregation: t.aggregation,
          title: t.title?.trim() ? t.title.trim() : null,
        })),
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
        <Button variant="ghost" size="icon" onClick={() => navigate(-1)}>
          <ArrowLeft className="size-4" />
        </Button>
        <div>
          <h1 className="text-lg font-semibold">
            {isEditing ? 'Edit dashboard' : 'New dashboard'}
          </h1>
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
            <Button
              type="button"
              variant="outline"
              onClick={() => navigate(-1)}
            >
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
  onRemoveSection,
  canRemoveSection,
}: {
  control: Control<DashboardFormData>
  sectionIndex: number
  metricOptions: { value: string; label: string }[]
  metricsLoading: boolean
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
            <div
              key={tileField.id}
              className="flex flex-col gap-2 rounded-md border border-border/60 p-3 sm:flex-row sm:items-start"
            >
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
                onClick={() => tilesArray.remove(tileIndex)}
                aria-label="Remove tile"
              >
                <Trash2 className="size-4" />
              </Button>
            </div>
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
              metric_name: '',
              aggregation: 'avg',
              title: '',
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
