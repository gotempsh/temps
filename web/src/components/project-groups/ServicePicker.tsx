// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState } from 'react'
import { RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { useDebounce } from '@/hooks/useDebounce'
import { getErrorMessage } from '@/utils/errorHandling'
import { useProjectGroups, useServiceSearch } from './useProjectGroups'

/** A picked service: enough to name it in messages. */
export interface PickedService {
  id: number
  name: string
}

interface ServicePickerProps {
  selected: PickedService[]
  onChange: (services: PickedService[]) => void
  /** Load only while the surrounding dialog is open. */
  enabled: boolean
  /** Services not offered, e.g. members of the project being edited. */
  excludeIds?: ReadonlySet<number>
  /** Shown when there is genuinely nothing to pick. */
  emptyMessage: string
}

/**
 * Checklist of services with a server-side filter, so every service can be
 * found however many there are. Services already in another project say so,
 * because picking one moves it.
 */
export function ServicePicker({
  selected,
  onChange,
  enabled,
  excludeIds,
  emptyMessage,
}: ServicePickerProps) {
  const [filter, setFilter] = useState('')
  const search = useDebounce(filter, 250)
  const query = useServiceSearch(search, enabled)
  const { groupByProjectId } = useProjectGroups()

  const candidates = useMemo(
    () =>
      query.services
        .filter((project) => !excludeIds?.has(project.id))
        .map((project) => ({
          project,
          currentGroup: groupByProjectId.get(project.id)?.name,
        }))
        // Standalone services first: they are the likelier picks.
        .sort(
          (a, b) =>
            Number(Boolean(a.currentGroup)) - Number(Boolean(b.currentGroup))
        ),
    [excludeIds, groupByProjectId, query.services]
  )
  const selectedIds = useMemo(
    () => new Set(selected.map((s) => s.id)),
    [selected]
  )

  const toggle = (service: PickedService) =>
    onChange(
      selectedIds.has(service.id)
        ? selected.filter((s) => s.id !== service.id)
        : [...selected, service]
    )

  const searching = search.trim() !== ''
  // Nothing to pick at all, as opposed to nothing matching the filter.
  const nothingToPick =
    query.isSuccess && !searching && !query.truncated && candidates.length === 0

  if (query.isError && !query.data) {
    return (
      <div
        role="alert"
        className="flex items-center gap-3 rounded-md border border-destructive/40 px-3 py-3 text-sm"
      >
        <p className="flex-1 text-destructive">
          {getErrorMessage(query.error, 'Services could not be loaded.')}
        </p>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={query.isFetching}
          onClick={() => void query.refetch()}
        >
          <RefreshCw
            className={query.isFetching ? 'size-4 animate-spin' : 'size-4'}
          />
          Retry
        </Button>
      </div>
    )
  }

  if (nothingToPick) {
    return (
      <p className="rounded-md border border-dashed px-3 py-6 text-center text-sm text-muted-foreground">
        {emptyMessage}
      </p>
    )
  }

  return (
    <div className="space-y-2">
      <Input
        placeholder="Filter services…"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        aria-label="Filter services"
      />
      <div
        className="max-h-64 divide-y overflow-y-auto rounded-md border"
        aria-busy={query.isLoading}
      >
        {query.isLoading ? (
          <PickerSkeleton />
        ) : (
          candidates.map(({ project, currentGroup }) => {
            const id = `pick-service-${project.id}`
            return (
              <label
                key={project.id}
                htmlFor={id}
                className="flex cursor-pointer items-center gap-3 px-3 py-2 hover:bg-muted/50"
              >
                <Checkbox
                  id={id}
                  checked={selectedIds.has(project.id)}
                  onCheckedChange={() =>
                    toggle({ id: project.id, name: project.name })
                  }
                />
                <ServiceIcon service={project} size="sm" />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-medium">
                    {project.name}
                  </span>
                  {currentGroup && (
                    <span className="block truncate text-xs text-muted-foreground">
                      In {currentGroup} · selecting it moves it here
                    </span>
                  )}
                </span>
              </label>
            )
          })
        )}
        {query.isSuccess && candidates.length === 0 && searching && (
          <p className="px-3 py-4 text-center text-sm text-muted-foreground">
            No service matches “{search.trim()}”.
          </p>
        )}
      </div>
      <p className="text-xs text-muted-foreground">
        {selected.length > 0 && `${selected.length} selected · `}
        {query.truncated
          ? `Showing ${query.services.length} of ${query.total} services. Type to find the rest.`
          : query.isSuccess
            ? `${query.total} ${query.total === 1 ? 'service' : 'services'}${searching ? (query.total === 1 ? ' matches' : ' match') : ''}`
            : null}
      </p>
    </div>
  )
}

function PickerSkeleton() {
  return (
    <>
      {[0, 1, 2].map((i) => (
        <div key={i} className="flex items-center gap-3 px-3 py-2">
          <Skeleton className="size-4 rounded" />
          <Skeleton className="size-6 rounded-md" />
          <Skeleton className="h-4 w-40" />
        </div>
      ))}
    </>
  )
}
