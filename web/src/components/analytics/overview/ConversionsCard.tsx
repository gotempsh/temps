// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Conversions on the analytics overview (ADR 109 D2).
 *
 * Lists every conversion goal with its conversions, conversion rate and
 * revenue in the selected period. Selecting one shows which sources, pages,
 * countries or devices its converting visitors came from. With no goals the
 * card stays, explains what a conversion is and offers to define one.
 */

import {
  createAnalyticsGoalMutation,
  deleteAnalyticsGoalMutation,
  getAnalyticsGoalBreakdownOptions,
  getAnalyticsGoalStatsOptions,
  listAnalyticsGoalsOptions,
  updateAnalyticsGoalMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  AnalyticsGoal,
  AnalyticsGoalBreakdownDimension,
  AnalyticsGoalStats,
  ProjectResponse,
} from '@/api/client/types.gen'
import { RankedList } from '@/components/data-display/RankedList'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
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
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
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
import { problemDetail } from '@/lib/api-problem'
import {
  BREAKDOWN_DIMENSIONS,
  EMPTY_CONVERSION_FORM,
  conversionFormSchema,
  describeGoal,
  formValuesToInput,
  formatRate,
  formatRevenue,
  goalToFormValues,
  type ConversionFormValues,
} from '@/lib/conversions'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { MoreHorizontal, Plus, Target } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'

const DOCS_URL = 'https://temps.sh/docs/analytics#conversions'

interface ConversionsCardProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

function useInvalidateConversions() {
  const queryClient = useQueryClient()
  return () =>
    Promise.all(
      [
        'listAnalyticsGoals',
        'getAnalyticsGoalStats',
        'getAnalyticsGoalBreakdown',
      ].map((id) => queryClient.invalidateQueries({ queryKey: [{ _id: id }] }))
    )
}

export function ConversionsCard({
  project,
  startDate,
  endDate,
  environment,
}: ConversionsCardProps) {
  const [selectedId, setSelectedId] = useState<number | null>(null)
  const [editing, setEditing] = useState<AnalyticsGoal | null>(null)
  const [dialogOpen, setDialogOpen] = useState(false)
  const [deleting, setDeleting] = useState<AnalyticsGoal | null>(null)
  const [dimension, setDimension] =
    useState<AnalyticsGoalBreakdownDimension>('channel')
  const invalidate = useInvalidateConversions()

  const goals = useQuery({
    ...listAnalyticsGoalsOptions({
      path: { project_id: project.id },
      query: { page_size: 100 },
    }),
  })
  const stats = useQuery({
    ...getAnalyticsGoalStatsOptions({
      path: { project_id: project.id },
      query: {
        start_date: startDate?.toISOString() ?? '',
        end_date: endDate?.toISOString() ?? '',
        environment_id: environment,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const deleteGoal = useMutation({
    ...deleteAnalyticsGoalMutation(),
    onSuccess: async () => {
      toast.success(`Conversion "${deleting?.name}" deleted`)
      if (deleting?.id === selectedId) setSelectedId(null)
      setDeleting(null)
      await invalidate()
    },
    onError: (error) =>
      toast.error(problemDetail(error, 'Could not delete the conversion')),
  })

  const definitions = new Map(
    (goals.data?.items ?? []).map((goal) => [goal.id, goal])
  )
  const rows = stats.data?.goals ?? []
  const hasRevenue = rows.some((row) => row.revenue)
  const selected = rows.find((row) => row.goal_id === selectedId) ?? null

  const openCreate = () => {
    setEditing(null)
    setDialogOpen(true)
  }

  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-4 space-y-0">
        <div className="min-w-0">
          <CardTitle className="flex items-center gap-2">
            <Target className="h-4 w-4" aria-hidden="true" />
            Conversions
          </CardTitle>
          <CardDescription>
            Visitors who reached a goal, out of{' '}
            {(stats.data?.visitors ?? 0).toLocaleString('en-US')} visitors in
            this period
          </CardDescription>
        </div>
        <Button size="sm" variant="outline" onClick={openCreate}>
          <Plus className="h-4 w-4" aria-hidden="true" />
          <span className="max-sm:sr-only">Add conversion</span>
        </Button>
      </CardHeader>
      <CardContent className="space-y-4">
        <ConversionsBody
          isLoading={stats.isLoading || goals.isLoading}
          // Without the definitions, Edit and Delete would have nothing to act on.
          isError={stats.isError || goals.isError}
          rows={rows}
          hasRevenue={hasRevenue}
          selectedId={selectedId}
          onSelect={(id) => setSelectedId(id === selectedId ? null : id)}
          onCreate={openCreate}
          onEdit={(id) => {
            const goal = definitionOrExplain(definitions, id)
            if (!goal) return
            setEditing(goal)
            setDialogOpen(true)
          }}
          onDelete={(id) => {
            const goal = definitionOrExplain(definitions, id)
            if (goal) setDeleting(goal)
          }}
          describe={(id) => {
            const goal = definitions.get(id)
            return goal ? describeGoal(goal) : ''
          }}
        />
        {selected && startDate && endDate && (
          <ConversionBreakdown
            projectId={project.id}
            goal={selected}
            dimension={dimension}
            onDimensionChange={setDimension}
            startDate={startDate}
            endDate={endDate}
            environment={environment}
          />
        )}
      </CardContent>

      <ConversionDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        projectId={project.id}
        goal={editing}
        onSaved={invalidate}
      />
      <ConfirmDeleteDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        resourceType="conversion"
        resourceName={deleting?.name ?? ''}
        consequences="The conversion stops appearing in reports. Recorded events are kept, so defining it again brings its history back."
        isPending={deleteGoal.isPending}
        onConfirm={() =>
          deleting &&
          deleteGoal.mutate({
            path: { project_id: project.id, goal_id: deleting.id },
          })
        }
      />
    </Card>
  )
}

/**
 * The definition of a reported goal, or `undefined` after telling the user
 * why: a goal deleted elsewhere since the list loaded has stats but no
 * definition.
 */
function definitionOrExplain(
  definitions: Map<number, AnalyticsGoal>,
  id: number
): AnalyticsGoal | undefined {
  const goal = definitions.get(id)
  if (!goal) {
    toast.error(
      `Conversion ${id} is no longer defined. Refresh the page to see the current conversions.`
    )
  }
  return goal
}

interface ConversionsBodyProps {
  isLoading: boolean
  isError: boolean
  rows: AnalyticsGoalStats[]
  hasRevenue: boolean
  selectedId: number | null
  onSelect: (id: number) => void
  onCreate: () => void
  onEdit: (id: number) => void
  onDelete: (id: number) => void
  describe: (id: number) => string
}

function ConversionsBody(props: ConversionsBodyProps) {
  if (props.isLoading) {
    return (
      <div className="space-y-3 py-2">
        {['a', 'b', 'c'].map((key) => (
          <Skeleton key={key} className="h-8 w-full" />
        ))}
      </div>
    )
  }
  if (props.isError) {
    return (
      <p className="py-4 text-sm text-muted-foreground">
        Conversions could not be loaded. Try refreshing the page.
      </p>
    )
  }
  if (props.rows.length === 0) {
    return (
      <div className="space-y-2 py-2 text-sm text-muted-foreground">
        <p>
          No conversions defined yet. A conversion is an event or a page that
          counts as success, for example the <code>signup</code> event or a
          visit to <code>/thank-you</code>. Temps then shows how many visitors
          reach it, the conversion rate, revenue, and where those visitors came
          from. It applies to events already recorded.
        </p>
        <div className="flex flex-wrap gap-x-4 gap-y-1">
          <Button
            variant="link"
            className="h-auto p-0"
            onClick={props.onCreate}
          >
            Define your first conversion
          </Button>
          <a
            className="text-primary hover:underline"
            href={DOCS_URL}
            target="_blank"
            rel="noreferrer"
          >
            How conversions work
          </a>
        </div>
      </div>
    )
  }
  return (
    <Table aria-label="Conversions">
      <TableHeader>
        <TableRow>
          <TableHead>Conversion</TableHead>
          <TableHead className="text-right">Visitors</TableHead>
          <TableHead className="text-right max-sm:hidden">Total</TableHead>
          <TableHead className="text-right">Rate</TableHead>
          {props.hasRevenue && (
            <TableHead className="text-right max-sm:hidden">Revenue</TableHead>
          )}
          <TableHead className="w-10">
            <span className="sr-only">Actions</span>
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {props.rows.map((row) => (
          <TableRow
            key={row.goal_id}
            data-state={
              row.goal_id === props.selectedId ? 'selected' : undefined
            }
          >
            <TableCell className="min-w-0">
              <button
                type="button"
                className="text-left hover:underline"
                aria-expanded={row.goal_id === props.selectedId}
                onClick={() => props.onSelect(row.goal_id)}
              >
                <span className="font-medium">{row.name}</span>
                <span className="block text-xs text-muted-foreground">
                  {props.describe(row.goal_id)}
                </span>
              </button>
            </TableCell>
            <TableCell className="text-right tabular-nums">
              {row.unique_conversions.toLocaleString('en-US')}
            </TableCell>
            <TableCell className="text-right tabular-nums max-sm:hidden">
              {row.total_conversions.toLocaleString('en-US')}
            </TableCell>
            <TableCell className="text-right tabular-nums">
              {formatRate(row.conversion_rate)}
            </TableCell>
            {props.hasRevenue && (
              <TableCell className="text-right tabular-nums max-sm:hidden">
                {row.revenue
                  ? formatRevenue(row.revenue.total, row.revenue.currency)
                  : '—'}
              </TableCell>
            )}
            <TableCell>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={`Actions for ${row.name}`}
                  >
                    <MoreHorizontal className="h-4 w-4" aria-hidden="true" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  <DropdownMenuItem onSelect={() => props.onEdit(row.goal_id)}>
                    Edit
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    className="text-destructive"
                    onSelect={() => props.onDelete(row.goal_id)}
                  >
                    Delete
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

function ConversionBreakdown({
  projectId,
  goal,
  dimension,
  onDimensionChange,
  startDate,
  endDate,
  environment,
}: {
  projectId: number
  goal: AnalyticsGoalStats
  dimension: AnalyticsGoalBreakdownDimension
  onDimensionChange: (dimension: AnalyticsGoalBreakdownDimension) => void
  startDate: Date
  endDate: Date
  environment: number | undefined
}) {
  const breakdown = useQuery({
    ...getAnalyticsGoalBreakdownOptions({
      path: { project_id: projectId, goal_id: goal.goal_id },
      query: {
        dimension,
        start_date: startDate.toISOString(),
        end_date: endDate.toISOString(),
        environment_id: environment,
        limit: 10,
      },
    }),
  })
  const label =
    BREAKDOWN_DIMENSIONS.find((d) => d.value === dimension)?.label ?? dimension

  return (
    <section className="space-y-3" aria-label={`${goal.name} by ${label}`}>
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <h3 className="text-sm font-medium">
          Visitors who converted on {goal.name}, by {label.toLowerCase()}
        </h3>
        <Select
          value={dimension}
          onValueChange={(value) =>
            onDimensionChange(value as AnalyticsGoalBreakdownDimension)
          }
        >
          <SelectTrigger className="w-full sm:w-[200px]" aria-label="Group by">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {BREAKDOWN_DIMENSIONS.map((d) => (
              <SelectItem key={d.value} value={d.value}>
                {d.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      {breakdown.isLoading ? (
        <Skeleton className="h-24 w-full" />
      ) : breakdown.isError ? (
        <p className="text-sm text-muted-foreground">
          The breakdown could not be loaded.
        </p>
      ) : (breakdown.data?.items.length ?? 0) === 0 ? (
        <p className="text-sm text-muted-foreground">
          No visitor converted on {goal.name} in this period.
        </p>
      ) : (
        <RankedList
          label={`${goal.name} by ${label}`}
          items={(breakdown.data?.items ?? []).map((row) => ({
            id: row.value ?? '(not set)',
            title: row.value ?? '(not set)',
            facts: [
              {
                label: 'Converted',
                value: row.unique_conversions.toLocaleString('en-US'),
              },
              {
                label: 'Total',
                value: row.total_conversions.toLocaleString('en-US'),
              },
              {
                label: 'Visitors',
                value: row.visitors.toLocaleString('en-US'),
              },
              { label: 'Rate', value: formatRate(row.conversion_rate) },
              ...(row.revenue
                ? [
                    {
                      label: 'Revenue',
                      value: formatRevenue(
                        row.revenue.total,
                        row.revenue.currency
                      ),
                    },
                  ]
                : []),
            ],
          }))}
        />
      )}
    </section>
  )
}

function ConversionDialog({
  open,
  onOpenChange,
  projectId,
  goal,
  onSaved,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  projectId: number
  goal: AnalyticsGoal | null
  onSaved: () => Promise<unknown>
}) {
  const form = useForm<ConversionFormValues>({
    resolver: zodResolver(conversionFormSchema),
    defaultValues: EMPTY_CONVERSION_FORM,
  })
  useEffect(() => {
    if (open) form.reset(goal ? goalToFormValues(goal) : EMPTY_CONVERSION_FORM)
  }, [open, goal, form])

  const onSuccess = async (saved: AnalyticsGoal) => {
    toast.success(`Conversion "${saved.name}" saved`)
    onOpenChange(false)
    await onSaved()
  }
  const onError = (error: unknown) =>
    toast.error(problemDetail(error, 'Could not save the conversion'))
  const create = useMutation({
    ...createAnalyticsGoalMutation(),
    onSuccess,
    onError,
  })
  const update = useMutation({
    ...updateAnalyticsGoalMutation(),
    onSuccess,
    onError,
  })
  const kind = form.watch('kind')

  const submit = form.handleSubmit((values) => {
    const body = formValuesToInput(values, goal ?? undefined)
    if (goal) {
      update.mutate({
        path: { project_id: projectId, goal_id: goal.id },
        body,
      })
    } else {
      create.mutate({ path: { project_id: projectId }, body })
    }
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {goal ? `Edit ${goal.name}` : 'Add a conversion'}
          </DialogTitle>
          <DialogDescription>
            Count the visitors who did something that matters. Applies to events
            already recorded too.
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form onSubmit={submit} className="space-y-4">
            <FormField
              control={form.control}
              name="name"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Name</FormLabel>
                  <FormControl>
                    <Input placeholder="Signup" {...field} />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="kind"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Counts when a visitor</FormLabel>
                  <FormControl>
                    <RadioGroup
                      value={field.value}
                      onValueChange={field.onChange}
                      className="grid grid-cols-1 gap-2 sm:grid-cols-2"
                    >
                      <KindCard
                        value="event"
                        title="Triggers an event"
                        example="signup, purchase, outbound_click"
                      />
                      <KindCard
                        value="page"
                        title="Visits a page"
                        example="/thank-you or /docs/*"
                      />
                    </RadioGroup>
                  </FormControl>
                </FormItem>
              )}
            />
            {kind === 'event' ? (
              <>
                <FormField
                  control={form.control}
                  name="eventName"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Event name</FormLabel>
                      <FormControl>
                        <Input placeholder="signup" {...field} />
                      </FormControl>
                      <FormDescription>
                        The name passed to <code>track()</code>, or a built-in
                        event such as <code>file_download</code>.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                  <FormField
                    control={form.control}
                    name="propertyKey"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Only when property (optional)</FormLabel>
                        <FormControl>
                          <Input placeholder="plan" {...field} />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <FormField
                    control={form.control}
                    name="propertyValue"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>equals</FormLabel>
                        <FormControl>
                          <Input placeholder="pro" {...field} />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </div>
                <div className="grid grid-cols-1 gap-2 sm:grid-cols-[1fr_120px]">
                  <FormField
                    control={form.control}
                    name="revenueProperty"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Revenue property (optional)</FormLabel>
                        <FormControl>
                          <Input placeholder="amount" {...field} />
                        </FormControl>
                        <FormDescription>
                          An event property holding a number, e.g.{' '}
                          <code>{"track('purchase', { amount: 49 })"}</code>.
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <FormField
                    control={form.control}
                    name="revenueCurrency"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Currency</FormLabel>
                        <FormControl>
                          <Input placeholder="EUR" maxLength={3} {...field} />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </div>
              </>
            ) : (
              <FormField
                control={form.control}
                name="pagePath"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Page path</FormLabel>
                    <FormControl>
                      <Input placeholder="/thank-you" {...field} />
                    </FormControl>
                    <FormDescription>
                      <code>*</code> matches anything, e.g. <code>/blog/*</code>
                      .
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            )}
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
              >
                Cancel
              </Button>
              <Button
                type="submit"
                disabled={create.isPending || update.isPending}
              >
                {goal ? 'Save' : 'Add conversion'}
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}

function KindCard({
  value,
  title,
  example,
}: {
  value: string
  title: string
  example: string
}) {
  const id = `conversion-kind-${value}`
  return (
    <label
      htmlFor={id}
      className="flex cursor-pointer items-start gap-3 rounded-md border p-3 has-[[data-state=checked]]:border-primary"
    >
      <RadioGroupItem id={id} value={value} className="mt-0.5" />
      <span>
        <span className="block text-sm font-medium">{title}</span>
        <span className="block text-xs text-muted-foreground">{example}</span>
      </span>
    </label>
  )
}
