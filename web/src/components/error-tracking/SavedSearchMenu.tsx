// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useRef, useState } from 'react'
import { zodResolver } from '@hookform/resolvers/zod'
import { Controller, useForm, useWatch } from 'react-hook-form'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Bookmark, BookmarkCheck, Trash2, Users } from 'lucide-react'
import {
  createErrorSavedSearchMutation,
  deleteErrorSavedSearchMutation,
  listErrorSavedSearchesOptions,
  listErrorSavedSearchesQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { ErrorSavedSearchResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
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
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { queryKeyPrefix } from '@/lib/refresh'
import {
  filtersFromView,
  viewFromFilters,
  viewMatches,
  type GroupListView,
} from './saved-search-view'
import {
  MAX_SAVED_SEARCH_NAME_CHARS,
  savedSearchFormSchema,
  type SavedSearchFormData,
} from './forms'

interface SavedSearchMenuProps {
  projectId: number
  view: GroupListView
  onApply: (view: GroupListView) => void
  /** Administrators may delete a shared search. */
  canModerate: boolean
}

/**
 * Saved searches over the error list (ADR 111, D1.8): open one, save the
 * current filters under a name, share it with the project, or delete it.
 */
export function SavedSearchMenu({
  projectId,
  view,
  onApply,
  canModerate,
}: SavedSearchMenuProps) {
  const queryClient = useQueryClient()
  const [saving, setSaving] = useState(false)
  const form = useForm<SavedSearchFormData>({
    resolver: zodResolver(savedSearchFormSchema),
    defaultValues: { name: '', shared: false },
  })
  const nameIsBlank =
    useWatch({ control: form.control, name: 'name' }).trim().length === 0
  const nameError = form.formState.errors.name?.message
  const [deleting, setDeleting] = useState<ErrorSavedSearchResponse | null>(
    null
  )
  const triggerRef = useRef<HTMLButtonElement>(null)
  const path = { project_id: projectId }

  const searches = useQuery({
    ...listErrorSavedSearchesOptions({
      path,
      query: { page: 1, page_size: 100 },
    }),
    staleTime: 30_000,
  })
  const refresh = () =>
    queryClient.invalidateQueries({
      queryKey: queryKeyPrefix(listErrorSavedSearchesQueryKey, { path }),
    })

  const create = useMutation({
    ...createErrorSavedSearchMutation(),
    meta: { errorTitle: 'Failed to save the search' },
    onSuccess: () => {
      setSaving(false)
      form.reset({ name: '', shared: false })
      refresh()
    },
  })
  const remove = useMutation({
    ...deleteErrorSavedSearchMutation(),
    meta: { errorTitle: 'Failed to delete the saved search' },
    onSuccess: refresh,
    onSettled: () => setDeleting(null),
  })

  const list = searches.data?.data ?? []
  const active = list.find((search) => viewMatches(view, search.filters))
  const mine = list.filter((search) => search.is_owner)
  const others = list.filter((search) => !search.is_owner)

  const item = (search: ErrorSavedSearchResponse) => (
    <DropdownMenuItem
      key={search.id}
      onClick={() => onApply(viewFromFilters(search.filters))}
      className="justify-between gap-2"
    >
      <span className="flex min-w-0 items-center gap-2">
        {search.shared && (
          <Users
            className="h-3.5 w-3.5 shrink-0 text-muted-foreground"
            aria-label="Shared"
          />
        )}
        <span className="truncate">{search.name}</span>
        {!search.is_owner && search.owner_name && (
          <span className="truncate text-xs text-muted-foreground">
            {search.owner_name}
          </span>
        )}
      </span>
      {(search.is_owner || (canModerate && search.shared)) && (
        <button
          type="button"
          className="rounded p-0.5 text-muted-foreground hover:text-destructive"
          aria-label={`Delete saved search ${search.name}`}
          onClick={(e) => {
            e.stopPropagation()
            setDeleting(search)
          }}
        >
          <Trash2 className="h-3.5 w-3.5" />
        </button>
      )}
    </DropdownMenuItem>
  )

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            ref={triggerRef}
            variant="outline"
            size="sm"
            className="h-9 gap-2"
            aria-label="Saved searches"
          >
            {active ? (
              <BookmarkCheck className="h-4 w-4" />
            ) : (
              <Bookmark className="h-4 w-4" />
            )}
            <span className="max-w-[140px] truncate">
              {active ? active.name : 'Saved searches'}
            </span>
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-72">
          <DropdownMenuItem onClick={() => setSaving(true)}>
            Save the current filters…
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          {searches.isLoading && (
            <p className="px-2 py-1.5 text-xs text-muted-foreground">
              Loading…
            </p>
          )}
          {searches.isError && (
            <p className="px-2 py-1.5 text-xs text-destructive">
              Could not load saved searches.
            </p>
          )}
          {!searches.isLoading && !searches.isError && list.length === 0 && (
            <p className="px-2 py-1.5 text-xs text-muted-foreground">
              No saved searches yet. Set the filters you check often, for
              example unresolved and assigned to me, and save them here.
            </p>
          )}
          {mine.length > 0 && (
            <>
              <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
                Yours
              </DropdownMenuLabel>
              {mine.map(item)}
            </>
          )}
          {others.length > 0 && (
            <>
              <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
                Shared with the project
              </DropdownMenuLabel>
              {others.map(item)}
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>

      <ConfirmDeleteDialog
        open={deleting !== null}
        onOpenChange={(open) => {
          if (!open) setDeleting(null)
        }}
        resourceType="saved search"
        resourceName={deleting?.name ?? ''}
        consequences={
          deleting?.shared
            ? 'It is shared, so it disappears for everyone in the project. The issues it matched are not changed.'
            : 'The issues it matched are not changed.'
        }
        isPending={remove.isPending}
        returnFocusRef={triggerRef}
        onConfirm={() => {
          if (deleting) {
            remove.mutate({ path: { ...path, saved_search_id: deleting.id } })
          }
        }}
      />

      <Dialog open={saving} onOpenChange={setSaving}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Save this search</DialogTitle>
            <DialogDescription>
              Saves the status, assignee, environment, text and sort of the
              list. The time range is not saved. &ldquo;Assigned to me&rdquo;
              means each person&rsquo;s own groups when a search is shared.
            </DialogDescription>
          </DialogHeader>
          <form
            className="space-y-4"
            noValidate
            onSubmit={form.handleSubmit(({ name, shared }) =>
              create.mutate({
                path,
                body: { name, shared, filters: filtersFromView(view) },
              })
            )}
          >
            <div className="space-y-2">
              <Label htmlFor="saved-search-name">Name</Label>
              <Input
                id="saved-search-name"
                placeholder="Open and mine"
                maxLength={MAX_SAVED_SEARCH_NAME_CHARS}
                autoFocus
                aria-invalid={nameError ? true : undefined}
                aria-describedby={
                  nameError ? 'saved-search-name-error' : undefined
                }
                {...form.register('name')}
              />
              {nameError && (
                <p
                  id="saved-search-name-error"
                  className="text-xs text-destructive"
                >
                  {nameError}
                </p>
              )}
            </div>
            <div className="flex items-center gap-2">
              <Controller
                control={form.control}
                name="shared"
                render={({ field }) => (
                  <Checkbox
                    id="saved-search-shared"
                    checked={field.value}
                    onCheckedChange={(checked) =>
                      field.onChange(checked === true)
                    }
                  />
                )}
              />
              <Label htmlFor="saved-search-shared" className="font-normal">
                Share with everyone who can open this project
              </Label>
            </div>
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => setSaving(false)}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={nameIsBlank || create.isPending}>
                Save
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </>
  )
}
