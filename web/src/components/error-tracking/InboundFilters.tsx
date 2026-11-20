// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createErrorInboundFilterMutation,
  deleteErrorInboundFilterMutation,
  listErrorInboundFiltersOptions,
  listErrorInboundFiltersQueryKey,
  updateErrorInboundFilterMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { InboundFilterResponse } from '@/api/client/types.gen'
import { PageHeader } from '@/components/layout/PageContainer'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Filter, Trash2 } from 'lucide-react'
import { useState } from 'react'
import {
  addableInboundFilterKinds,
  inboundFilterKindInfo,
  type InboundFilterKindName,
} from './inbound-filter-kinds'

interface InboundFiltersProps {
  projectId: number
}

/**
 * Server-side inbound filters (ADR 111, D1.4): drop noisy errors before they
 * are stored, and show how many each rule dropped so a rule that eats real
 * errors is visible.
 */
export function InboundFilters({ projectId }: InboundFiltersProps) {
  const queryClient = useQueryClient()
  const path = { project_id: projectId }
  const { data, isLoading } = useQuery({
    ...listErrorInboundFiltersOptions({ path }),
  })
  const invalidate = () =>
    queryClient.invalidateQueries({
      queryKey: listErrorInboundFiltersQueryKey({ path }),
    })

  const filters = data?.filters ?? []
  const addable = addableInboundFilterKinds(filters)
  const [kind, setKind] = useState<InboundFilterKindName | ''>('')
  const [pattern, setPattern] = useState('')
  const [deleting, setDeleting] = useState<InboundFilterResponse | null>(null)
  const kindInfo = kind ? inboundFilterKindInfo(kind) : undefined

  const create = useMutation({
    ...createErrorInboundFilterMutation(),
    meta: { errorTitle: 'Failed to add the inbound filter' },
    onSuccess: () => {
      setKind('')
      setPattern('')
      invalidate()
    },
  })
  const update = useMutation({
    ...updateErrorInboundFilterMutation(),
    meta: { errorTitle: 'Failed to update the inbound filter' },
    onSuccess: invalidate,
  })
  const remove = useMutation({
    ...deleteErrorInboundFilterMutation(),
    meta: { errorTitle: 'Failed to delete the inbound filter' },
    onSuccess: () => {
      setDeleting(null)
      invalidate()
    },
  })

  const canAdd =
    !!kindInfo && (!kindInfo.takesPattern || pattern.trim().length > 0)
  const atLimit = data ? filters.length >= data.limit : false

  return (
    <div className="space-y-4">
      <PageHeader
        title="Inbound filters"
        description="Drop noisy errors before they are stored: browser extensions, crawlers, localhost, a message pattern or an old release. Each rule shows how many events it dropped."
      />

      <Card>
        <CardContent className="space-y-3 pt-6">
          <div className="grid gap-3 sm:grid-cols-[minmax(0,14rem)_minmax(0,1fr)_auto] sm:items-end">
            <div className="space-y-1.5">
              <Label htmlFor="inbound-filter-kind">Drop</Label>
              <Select
                value={kind}
                onValueChange={(value) =>
                  setKind(value as InboundFilterKindName)
                }
                disabled={atLimit}
              >
                <SelectTrigger id="inbound-filter-kind">
                  <SelectValue placeholder="Choose what to drop" />
                </SelectTrigger>
                <SelectContent>
                  {addable.map((info) => (
                    <SelectItem key={info.kind} value={info.kind}>
                      {info.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="inbound-filter-pattern">Pattern</Label>
              <Input
                id="inbound-filter-pattern"
                value={pattern}
                maxLength={data?.max_pattern_chars ?? 200}
                onChange={(e) => setPattern(e.target.value)}
                disabled={!kindInfo?.takesPattern}
                placeholder={
                  kindInfo?.takesPattern
                    ? kindInfo.placeholder
                    : 'Not needed for this kind'
                }
              />
            </div>
            <Button
              disabled={!canAdd || create.isPending || atLimit}
              onClick={() =>
                kind &&
                create.mutate({
                  path,
                  body: {
                    kind,
                    pattern: kindInfo?.takesPattern ? pattern.trim() : null,
                  },
                })
              }
            >
              Add filter
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">
            {kindInfo
              ? kindInfo.description
              : 'A filter applies from the next event. Dropped events are never stored and cannot be recovered.'}
            {atLimit && data
              ? ` This project has reached the limit of ${data.limit} filters.`
              : ''}
          </p>
        </CardContent>
      </Card>

      {isLoading ? (
        <Skeleton className="h-32 w-full" />
      ) : filters.length === 0 ? (
        <EmptyState
          size="compact"
          icon={Filter}
          title="No inbound filters"
          description='Every error the SDKs send is stored. Add a filter above, for example "Browser extensions" or the message pattern *ResizeObserver loop*.'
        />
      ) : (
        <Card>
          <CardContent className="divide-y divide-border/60 p-0">
            {filters.map((filter) => {
              const info = inboundFilterKindInfo(filter.kind)
              return (
                <div
                  key={filter.id}
                  className="flex flex-col gap-2 px-4 py-3 sm:flex-row sm:items-center sm:justify-between"
                >
                  <div className="min-w-0 space-y-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="font-medium">
                        {info?.label ?? filter.kind}
                      </span>
                      {filter.pattern && (
                        <code className="rounded bg-muted px-1.5 py-0.5 text-xs break-all">
                          {filter.pattern}
                        </code>
                      )}
                      {!filter.enabled && <Badge variant="outline">Off</Badge>}
                    </div>
                    <p className="text-xs text-muted-foreground">
                      {filter.dropped_count.toLocaleString()} event
                      {filter.dropped_count === 1 ? '' : 's'} dropped
                      {filter.last_dropped_at && (
                        <>
                          {' '}
                          · last <TimeAgo date={filter.last_dropped_at} />
                        </>
                      )}
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-3">
                    <Switch
                      checked={filter.enabled}
                      aria-label={`${filter.enabled ? 'Turn off' : 'Turn on'} the ${info?.label ?? filter.kind} filter`}
                      disabled={update.isPending}
                      onCheckedChange={(enabled) =>
                        update.mutate({
                          path: { ...path, filter_id: filter.id },
                          body: { enabled },
                        })
                      }
                    />
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label={`Delete the ${info?.label ?? filter.kind} filter`}
                      onClick={() => setDeleting(filter)}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
              )
            })}
          </CardContent>
        </Card>
      )}

      <ConfirmDeleteDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        resourceType="inbound filter"
        resourceName={
          deleting
            ? `${inboundFilterKindInfo(deleting.kind)?.label ?? deleting.kind}${deleting.pattern ? ` ${deleting.pattern}` : ''}`
            : ''
        }
        consequences="Matching errors are stored again from the next event. Its dropped count is deleted with it."
        onConfirm={() =>
          deleting &&
          remove.mutate({ path: { ...path, filter_id: deleting.id } })
        }
        isPending={remove.isPending}
      />
    </div>
  )
}
