// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link } from 'react-router'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { z } from 'zod'
import { Bell, Bookmark, Pin, PinOff, Trash2 } from 'lucide-react'
import {
  createSavedSearchMutation,
  deleteSavedSearchMutation,
  getProjectOptions,
  listSavedSearchesOptions,
  listSavedSearchesQueryKey,
  updateSavedSearchMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  SavedSearchQuery,
  SavedSearchResponse,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { cn } from '@/lib/utils'
import { alertOnSearchHref, orderSavedSearches } from '@/lib/saved-log-search'

const saveSchema = z.object({
  name: z
    .string()
    .trim()
    .min(1, 'Name is required')
    .max(120, 'Name must be 120 characters or fewer'),
  description: z
    .string()
    .max(1000, 'Description must be 1000 characters or fewer'),
  pinned: z.boolean(),
})
type SaveForm = z.infer<typeof saveSchema>

export interface SavedLogSearchesProps {
  /** Saved searches belong to one service; `undefined` shows how to pick one. */
  projectId: number | undefined
  /** The filters on the page right now, as a saved search stores them. */
  currentQuery: SavedSearchQuery
  /** The relative range the page shows, under the server's names. */
  currentRange: string
  onOpen: (search: SavedSearchResponse) => void
}

/**
 * Saved searches for the logs page: open one, save the current filters,
 * pin, delete, or start an alert from one.
 */
export function SavedLogSearches({
  projectId,
  currentQuery,
  currentRange,
  onOpen,
}: SavedLogSearchesProps) {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const [saveOpen, setSaveOpen] = useState(false)
  const [deleteTarget, setDeleteTarget] = useState<SavedSearchResponse | null>(
    null
  )
  const path = { project_id: projectId ?? 0 }
  const searches = useQuery({
    ...listSavedSearchesOptions({ path, query: { page_size: 100 } }),
    enabled: !!projectId && open,
  })
  const project = useQuery({
    ...getProjectOptions({ path: { id: projectId ?? 0 } }),
    enabled: !!projectId && open,
    staleTime: 60_000,
  })
  const invalidate = () =>
    queryClient.invalidateQueries({
      queryKey: listSavedSearchesQueryKey({ path }),
    })

  const form = useForm<SaveForm>({
    resolver: zodResolver(saveSchema),
    defaultValues: { name: '', description: '', pinned: false },
  })
  const pinned = useWatch({ control: form.control, name: 'pinned' })
  const create = useMutation({
    ...createSavedSearchMutation(),
    meta: { errorTitle: 'Failed to save the search' },
    onSuccess: (saved) => {
      toast.success(`Saved "${saved.name}"`)
      setSaveOpen(false)
      form.reset()
      void invalidate()
    },
  })
  const update = useMutation({
    ...updateSavedSearchMutation(),
    meta: { errorTitle: 'Failed to update the saved search' },
    onSuccess: (saved) => {
      toast.success(
        saved.pinned ? `Pinned "${saved.name}"` : `Unpinned "${saved.name}"`
      )
      void invalidate()
    },
  })
  const remove = useMutation({
    ...deleteSavedSearchMutation(),
    meta: { errorTitle: 'Failed to delete the saved search' },
    onSuccess: () => {
      toast.success('Saved search deleted')
      setDeleteTarget(null)
      void invalidate()
    },
  })

  const submit = form.handleSubmit((values) => {
    if (!projectId) return
    create.mutate({
      path: { project_id: projectId },
      body: {
        name: values.name,
        description: values.description.trim() || null,
        query: currentQuery,
        time_range: currentRange,
        pinned: values.pinned,
      },
    })
  })

  const list = orderSavedSearches(searches.data?.data ?? [])

  return (
    <>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button size="sm" variant="outline" className="h-9 gap-2">
            <Bookmark className="size-4" />
            <span className="max-sm:sr-only">Saved searches</span>
          </Button>
        </PopoverTrigger>
        <PopoverContent align="end" className="w-[min(24rem,90vw)] p-0">
          {!projectId ? (
            <div className="space-y-1 p-4 text-sm">
              <p className="font-medium">Saved searches belong to a service</p>
              <p className="text-muted-foreground">
                Filter by a service (type <code>project:</code> in the search)
                to save this search, chart it on a dashboard or alert on it.
              </p>
            </div>
          ) : (
            <div className="flex flex-col">
              <div className="max-h-80 overflow-y-auto p-1">
                {searches.isPending ? (
                  <div className="space-y-1 p-2">
                    <Skeleton className="h-7 w-full" />
                    <Skeleton className="h-7 w-full" />
                  </div>
                ) : searches.error ? (
                  <div className="space-y-2 p-3 text-sm">
                    <p>Saved searches could not be loaded.</p>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => void searches.refetch()}
                    >
                      Retry
                    </Button>
                  </div>
                ) : list.length === 0 ? (
                  <p className="p-3 text-sm text-muted-foreground">
                    No saved searches in this service yet.
                  </p>
                ) : (
                  <ul aria-label="Saved searches">
                    {list.map((search) => (
                      <li
                        key={search.id}
                        className="group flex items-center gap-1 rounded-md px-1 hover:bg-muted/60"
                      >
                        <button
                          type="button"
                          className="min-w-0 flex-1 px-1 py-1.5 text-left text-sm"
                          onClick={() => {
                            setOpen(false)
                            onOpen(search)
                          }}
                        >
                          <span className="block truncate font-medium">
                            {search.name}
                          </span>
                          {search.description && (
                            <span className="block truncate text-xs text-muted-foreground">
                              {search.description}
                            </span>
                          )}
                        </button>
                        <Button
                          size="icon"
                          variant="ghost"
                          className="size-7"
                          aria-label={
                            search.pinned
                              ? `Unpin ${search.name}`
                              : `Pin ${search.name}`
                          }
                          title={search.pinned ? 'Unpin' : 'Pin to the top'}
                          disabled={update.isPending}
                          onClick={() =>
                            update.mutate({
                              path: {
                                project_id: search.project_id,
                                saved_search_id: search.id,
                              },
                              body: { pinned: !search.pinned },
                            })
                          }
                        >
                          {search.pinned ? (
                            <PinOff className="size-3.5" />
                          ) : (
                            <Pin className="size-3.5" />
                          )}
                        </Button>
                        {project.data && (
                          <Button
                            size="icon"
                            variant="ghost"
                            className="size-7"
                            asChild
                          >
                            <Link
                              to={alertOnSearchHref(
                                project.data.slug,
                                search.id
                              )}
                              aria-label={`Alert on ${search.name}`}
                              title="Alert on this search"
                            >
                              <Bell className="size-3.5" />
                            </Link>
                          </Button>
                        )}
                        <Button
                          size="icon"
                          variant="ghost"
                          className="size-7"
                          aria-label={`Delete ${search.name}`}
                          title={
                            search.alert_count > 0
                              ? `Used by ${search.alert_count} alert${search.alert_count === 1 ? '' : 's'}; remove ${search.alert_count === 1 ? 'it' : 'them'} first`
                              : 'Delete'
                          }
                          disabled={search.alert_count > 0}
                          onClick={() => setDeleteTarget(search)}
                        >
                          <Trash2 className="size-3.5" />
                        </Button>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
              <div className="border-t p-2">
                <Button
                  size="sm"
                  className="w-full"
                  onClick={() => {
                    setOpen(false)
                    setSaveOpen(true)
                  }}
                >
                  Save current search
                </Button>
              </div>
            </div>
          )}
        </PopoverContent>
      </Popover>

      <Dialog open={saveOpen} onOpenChange={setSaveOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Save this search</DialogTitle>
          </DialogHeader>
          <form onSubmit={submit} className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="saved-search-name">Name</Label>
              <Input
                id="saved-search-name"
                placeholder="e.g. Checkout errors"
                {...form.register('name')}
              />
              {form.formState.errors.name && (
                <p className="text-xs text-destructive">
                  {form.formState.errors.name.message}
                </p>
              )}
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="saved-search-description">
                Description (optional)
              </Label>
              <Input
                id="saved-search-description"
                {...form.register('description')}
              />
              {form.formState.errors.description && (
                <p className="text-xs text-destructive">
                  {form.formState.errors.description.message}
                </p>
              )}
            </div>
            <div className="flex items-center justify-between gap-4">
              <Label htmlFor="saved-search-pinned">Pin to the top</Label>
              <Switch
                id="saved-search-pinned"
                checked={pinned}
                onCheckedChange={(checked) => form.setValue('pinned', checked)}
              />
            </div>
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => setSaveOpen(false)}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={create.isPending}>
                Save
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>

      <ConfirmDeleteDialog
        open={deleteTarget !== null}
        onOpenChange={(next) => !next && setDeleteTarget(null)}
        resourceType="saved search"
        resourceName={deleteTarget?.name ?? ''}
        consequences="Dashboard panels that chart it show no data until they are pointed at another search."
        isPending={remove.isPending}
        onConfirm={() =>
          deleteTarget &&
          remove.mutate({
            path: {
              project_id: deleteTarget.project_id,
              saved_search_id: deleteTarget.id,
            },
          })
        }
      />
    </>
  )
}

/** Shown on the logs page when an opened search holds filters the page cannot show. */
export function SavedSearchNotShown({
  name,
  notShown,
  className,
}: {
  name: string
  notShown: string[]
  className?: string
}) {
  if (!notShown.length) return null
  return (
    <p className={cn('text-xs text-muted-foreground', className)}>
      “{name}” also filters on {notShown.join(', ')}; alerts and dashboard
      panels built from it apply those, this page does not.
    </p>
  )
}
