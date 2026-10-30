// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The project's own traffic filters: add a referrer, IP/range, network or
 * country, and review or remove existing ones. Inline "Exclude" actions on
 * reports create filters too; this is the one place to see them all.
 */

import type {
  TrafficFilterKind,
  TrafficFilterResponse,
} from '@/api/client/types.gen'
import {
  createTrafficFilterMutation,
  deleteTrafficFilterMutation,
  getFilteredTrafficQueryKey,
  listTrafficFiltersOptions,
  listTrafficFiltersQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { EmptyState } from '@/components/ui/empty-state'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { SettingsSection } from '@/components/ui/settings-section'
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
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import { Filter, Loader2, Plus, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { Controller, useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import {
  type EmptyFilterListState,
  TRAFFIC_FILTER_KINDS,
  TRAFFIC_FILTER_KIND_COPY,
  emptyFilterListState,
  formatTrafficFilterValue,
} from './traffic-filters-shared'

const PAGE_SIZE = 20

// Values are validated and normalised server-side (one parser for IPs, CIDR
// limits and domains); the form only rejects what is obviously empty, and the
// server's message is shown when a value is refused.
const addFilterSchema = z.object({
  kind: z.enum(['referrer', 'ip', 'asn', 'country']),
  value: z.string().trim().min(1, 'Enter a value to exclude'),
  note: z.string().max(500, 'A note is at most 500 characters').optional(),
})

type AddFilterForm = z.infer<typeof addFilterSchema>

type KindFilter = TrafficFilterKind | 'all'

interface TrafficFiltersSectionProps {
  projectId: number
}

export function TrafficFiltersSection({
  projectId,
}: TrafficFiltersSectionProps) {
  const queryClient = useQueryClient()
  const [page, setPage] = useState(1)
  const [kindFilter, setKindFilter] = useState<KindFilter>('all')
  const [pendingDelete, setPendingDelete] =
    useState<TrafficFilterResponse | null>(null)

  const list = useQuery(
    listTrafficFiltersOptions({
      path: { project_id: projectId },
      query: {
        kind: kindFilter === 'all' ? undefined : kindFilter,
        page,
        page_size: PAGE_SIZE,
      },
    })
  )

  // Takes the project from the mutation, not the render: the user may have
  // switched projects while the write was in flight.
  const invalidate = (mutatedProjectId: number) => {
    void queryClient.invalidateQueries({
      queryKey: listTrafficFiltersQueryKey({
        path: { project_id: mutatedProjectId },
      }),
    })
    void queryClient.invalidateQueries({
      queryKey: getFilteredTrafficQueryKey({
        path: { project_id: mutatedProjectId },
      }),
    })
  }

  const form = useForm<AddFilterForm>({
    resolver: zodResolver(addFilterSchema),
    defaultValues: { kind: 'referrer', value: '', note: '' },
  })
  const selectedKind = useWatch({ control: form.control, name: 'kind' })

  const create = useMutation({
    ...createTrafficFilterMutation(),
    meta: { errorTitle: 'Could not add the traffic filter' },
    onSuccess: (filter, variables) => {
      invalidate(variables.path.project_id)
      toast.success(
        `${formatTrafficFilterValue(filter.kind, filter.value)} is excluded from analytics`
      )
      form.reset({ kind: filter.kind, value: '', note: '' })
    },
  })

  const remove = useMutation({
    ...deleteTrafficFilterMutation(),
    meta: { errorTitle: 'Could not remove the traffic filter' },
    onSuccess: (_, variables) => {
      invalidate(variables.path.project_id)
      toast.success('Filter removed. Matching visits count again from now on.')
      setPendingDelete(null)
    },
  })

  const onSubmit = form.handleSubmit((values) =>
    create.mutate({
      path: { project_id: projectId },
      body: {
        kind: values.kind,
        value: values.value,
        note: values.note?.trim() || null,
      },
    })
  )

  const total = list.data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE))
  // Removing the last filter on the last page leaves it past the end: move to
  // the last page that still has rows rather than showing an empty one.
  if (list.data && page > totalPages) setPage(totalPages)

  return (
    <SettingsSection
      defaultOpen
      title="Your filters"
      description="Referrers, IPs, networks and countries you have chosen to leave out of this project's analytics."
      icon={Filter}
    >
      <div className="space-y-6">
        <form
          onSubmit={(event) => void onSubmit(event)}
          className="flex flex-col gap-2 lg:flex-row lg:items-start"
        >
          <div className="w-full space-y-1 lg:w-[180px]">
            <Label htmlFor="traffic-filter-kind" className="sr-only">
              Filter type
            </Label>
            <Controller
              control={form.control}
              name="kind"
              render={({ field }) => (
                <Select value={field.value} onValueChange={field.onChange}>
                  <SelectTrigger id="traffic-filter-kind">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {TRAFFIC_FILTER_KINDS.map((kind) => (
                      <SelectItem key={kind} value={kind}>
                        {TRAFFIC_FILTER_KIND_COPY[kind].label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            />
          </div>
          <div className="w-full flex-1 space-y-1">
            <Label htmlFor="traffic-filter-value" className="sr-only">
              Value
            </Label>
            <Input
              id="traffic-filter-value"
              placeholder={TRAFFIC_FILTER_KIND_COPY[selectedKind].placeholder}
              autoComplete="off"
              {...form.register('value')}
            />
            <p className="text-xs text-muted-foreground">
              {form.formState.errors.value?.message ??
                TRAFFIC_FILTER_KIND_COPY[selectedKind].help}
            </p>
          </div>
          <div className="w-full space-y-1 lg:w-[240px]">
            <Label htmlFor="traffic-filter-note" className="sr-only">
              Note
            </Label>
            <Input
              id="traffic-filter-note"
              placeholder="Note (optional)"
              autoComplete="off"
              {...form.register('note')}
            />
            {form.formState.errors.note && (
              <p className="text-xs text-destructive">
                {form.formState.errors.note.message}
              </p>
            )}
          </div>
          <Button
            type="submit"
            disabled={create.isPending}
            aria-label="Exclude from analytics"
          >
            {create.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin sm:mr-2" />
            ) : (
              <Plus className="h-4 w-4 sm:mr-2" />
            )}
            <span className="hidden sm:inline">Exclude</span>
          </Button>
        </form>

        <div className="space-y-3">
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
            <p className="text-sm text-muted-foreground">
              {list.data
                ? `${total} filter${total === 1 ? '' : 's'}${
                    kindFilter === 'all'
                      ? ''
                      : ` · ${TRAFFIC_FILTER_KIND_COPY[kindFilter].label}`
                  } · up to ${list.data.max_filters_per_kind} of each type`
                : ' '}
            </p>
            <Select
              value={kindFilter}
              onValueChange={(value) => {
                setKindFilter(value as KindFilter)
                setPage(1)
              }}
            >
              <SelectTrigger
                className="w-full sm:w-[180px]"
                aria-label="Show filters of type"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All types</SelectItem>
                {TRAFFIC_FILTER_KINDS.map((kind) => (
                  <SelectItem key={kind} value={kind}>
                    {TRAFFIC_FILTER_KIND_COPY[kind].label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <FilterTable
            isPending={list.isPending}
            // A failed background refresh keeps the rows already on screen.
            error={list.isError && !list.data ? list.error : null}
            onRetry={() => void list.refetch()}
            filters={list.data?.filters ?? []}
            emptyState={emptyFilterListState(kindFilter)}
            onClearKindFilter={() => {
              setKindFilter('all')
              setPage(1)
            }}
            onDelete={setPendingDelete}
            deleting={remove.isPending}
          />

          {total > PAGE_SIZE && (
            <ResponsivePagination
              page={page}
              pageSize={PAGE_SIZE}
              total={total}
              totalPages={totalPages}
              onPageChange={setPage}
              ariaLabel="Traffic filter pages"
            />
          )}
        </div>
      </div>

      <ConfirmDeleteDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open && !remove.isPending) setPendingDelete(null)
        }}
        resourceType="traffic filter"
        resourceName={
          pendingDelete
            ? formatTrafficFilterValue(pendingDelete.kind, pendingDelete.value)
            : ''
        }
        verb="Remove"
        irreversible={false}
        consequences="Matching visits count in analytics again from now on. Visits it already excluded stay excluded."
        isPending={remove.isPending}
        onConfirm={() => {
          if (!pendingDelete) return
          remove.mutate({
            path: {
              project_id: projectId,
              filter_id: pendingDelete.id,
            },
          })
        }}
      />
    </SettingsSection>
  )
}

interface FilterTableProps {
  isPending: boolean
  error: unknown
  onRetry: () => void
  filters: TrafficFilterResponse[]
  emptyState: EmptyFilterListState
  onClearKindFilter: () => void
  onDelete: (filter: TrafficFilterResponse) => void
  deleting: boolean
}

function FilterTable({
  isPending,
  error,
  onRetry,
  filters,
  emptyState,
  onClearKindFilter,
  onDelete,
  deleting,
}: FilterTableProps) {
  if (isPending) {
    return (
      <div className="space-y-2 rounded-md border p-4">
        {[0, 1, 2].map((row) => (
          <div key={row} className="flex items-center justify-between gap-4">
            <Skeleton className="h-4 w-48" />
            <Skeleton className="h-4 w-24" />
          </div>
        ))}
      </div>
    )
  }

  if (error) {
    return (
      <div className="flex flex-col items-start gap-2 rounded-md border border-destructive/40 p-4 text-sm">
        <p className="text-destructive">
          {problemDetail(error, 'Could not load the traffic filters.')}
        </p>
        <Button size="sm" variant="outline" onClick={onRetry}>
          Try again
        </Button>
      </div>
    )
  }

  if (filters.length === 0 && emptyState.kind === 'no-match') {
    return (
      <EmptyState
        size="compact"
        icon={Filter}
        title="No matching filters"
        description={`This project has no ${emptyState.noun} filters. Filters of other types still apply.`}
        action={
          <Button size="sm" variant="outline" onClick={onClearKindFilter}>
            Show all types
          </Button>
        }
      />
    )
  }

  if (filters.length === 0) {
    return (
      <EmptyState
        size="compact"
        icon={Filter}
        title="No traffic filters yet"
        description={
          <>
            Add one above, or use{' '}
            <span className="font-medium text-foreground">
              Exclude from analytics
            </span>{' '}
            on a referrer or a visitor.
          </>
        }
      />
    )
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Type</TableHead>
          <TableHead>Value</TableHead>
          <TableHead className="hidden md:table-cell">Note</TableHead>
          <TableHead className="hidden sm:table-cell">Added</TableHead>
          <TableHead className="w-12">
            <span className="sr-only">Actions</span>
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {filters.map((filter) => (
          <TableRow key={filter.id}>
            <TableCell>
              <Badge variant="outline">
                {TRAFFIC_FILTER_KIND_COPY[filter.kind].label}
              </Badge>
            </TableCell>
            <TableCell className="font-mono text-sm">
              {formatTrafficFilterValue(filter.kind, filter.value)}
              {filter.note && (
                <div className="font-sans text-xs text-muted-foreground md:hidden">
                  {filter.note}
                </div>
              )}
            </TableCell>
            <TableCell className="hidden max-w-[320px] truncate text-sm text-muted-foreground md:table-cell">
              {filter.note ?? '—'}
            </TableCell>
            <TableCell className="hidden text-sm text-muted-foreground sm:table-cell">
              {format(new Date(filter.created_at), 'MMM d, yyyy')}
            </TableCell>
            <TableCell>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={`Remove filter ${formatTrafficFilterValue(filter.kind, filter.value)}`}
                disabled={deleting}
                onClick={() => onDelete(filter)}
              >
                <Trash2 className="h-4 w-4" />
              </Button>
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}
