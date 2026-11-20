// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { zodResolver } from '@hookform/resolvers/zod'
import { useForm, useWatch } from 'react-hook-form'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { MessageSquare, Pencil, Trash2 } from 'lucide-react'
import {
  createErrorGroupCommentMutation,
  deleteErrorGroupCommentMutation,
  listErrorGroupActivityOptions,
  listErrorGroupActivityQueryKey,
  updateErrorGroupCommentMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ErrorGroupActivityResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { Textarea } from '@/components/ui/textarea'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { queryKeyPrefix } from '@/lib/refresh'
import {
  activityActor,
  commentPreview,
  describeActivity,
} from './activity-label'
import {
  commentFormSchema,
  MAX_COMMENT_CHARS,
  type CommentFormData,
} from './forms'

const PAGE_SIZE = 20

interface GroupActivityProps {
  projectId: number
  groupId: number
  currentUserId?: number
  /** Administrators may delete anyone's comment. */
  canModerate: boolean
}

/**
 * The comments and history of an error group (ADR 111, D1.7): who changed its
 * status, assigned, merged or split it, which event reopened it, and what the
 * team wrote about it. Newest first.
 */
export function GroupActivity({
  projectId,
  groupId,
  currentUserId,
  canModerate,
}: GroupActivityProps) {
  const queryClient = useQueryClient()
  const [page, setPage] = useState(1)
  const composer = useForm<CommentFormData>({
    resolver: zodResolver(commentFormSchema),
    defaultValues: { body: '' },
  })
  const path = { project_id: projectId, group_id: groupId }

  const activity = useQuery({
    ...listErrorGroupActivityOptions({
      path,
      query: { page, page_size: PAGE_SIZE },
    }),
  })
  const refresh = () =>
    queryClient.invalidateQueries({
      queryKey: queryKeyPrefix(listErrorGroupActivityQueryKey, { path }),
    })

  const create = useMutation({
    ...createErrorGroupCommentMutation(),
    meta: { errorTitle: 'Failed to add the comment' },
    onSuccess: () => {
      composer.reset({ body: '' })
      setPage(1)
      refresh()
    },
  })

  const entries = activity.data?.data ?? []
  const total = activity.data?.total_count ?? 0
  const pages = Math.max(1, Math.ceil(total / PAGE_SIZE))
  const draftIsBlank =
    useWatch({ control: composer.control, name: 'body' }).trim().length === 0
  const draftError = composer.formState.errors.body?.message

  return (
    <div className="space-y-4">
      <Card>
        <CardContent className="p-4">
          <form
            className="space-y-2"
            noValidate
            onSubmit={composer.handleSubmit(({ body }) =>
              create.mutate({ path, body: { body } })
            )}
          >
            <Textarea
              placeholder="Add a comment: what you found, a link to the fix, who is on it"
              aria-label="Comment on this error group"
              aria-invalid={draftError ? true : undefined}
              maxLength={MAX_COMMENT_CHARS}
              rows={3}
              {...composer.register('body')}
            />
            {draftError && (
              <p className="text-xs text-destructive">{draftError}</p>
            )}
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs text-muted-foreground">
                Everyone who can open this project sees comments.
              </span>
              <Button
                type="submit"
                size="sm"
                disabled={draftIsBlank || create.isPending}
              >
                <MessageSquare className="mr-2 h-4 w-4" />
                Comment
              </Button>
            </div>
          </form>
        </CardContent>
      </Card>

      {activity.isLoading && (
        <div className="space-y-2">
          <Skeleton className="h-12 w-full" />
          <Skeleton className="h-12 w-full" />
        </div>
      )}
      {activity.isError && (
        <Alert variant="destructive">
          <AlertDescription>
            Could not load the activity of this group.
          </AlertDescription>
        </Alert>
      )}
      {!activity.isLoading && !activity.isError && entries.length === 0 && (
        <p className="text-sm text-muted-foreground">
          Nothing has happened to this group yet. Status changes, assignments,
          merges, splits, regressions and comments show up here.
        </p>
      )}

      <ol className="space-y-2">
        {entries.map((entry) => (
          <ActivityRow
            key={entry.id}
            entry={entry}
            projectId={projectId}
            groupId={groupId}
            canEdit={
              entry.kind === 'comment' &&
              currentUserId !== undefined &&
              entry.actor_user_id === currentUserId
            }
            canDelete={
              entry.kind === 'comment' &&
              (canModerate ||
                (currentUserId !== undefined &&
                  entry.actor_user_id === currentUserId))
            }
            onChanged={refresh}
          />
        ))}
      </ol>

      {pages > 1 && (
        <ResponsivePagination
          page={page}
          pageSize={PAGE_SIZE}
          total={total}
          totalPages={pages}
          onPageChange={setPage}
          ariaLabel="Activity pages"
        />
      )}
    </div>
  )
}

function ActivityRow({
  entry,
  projectId,
  groupId,
  canEdit,
  canDelete,
  onChanged,
}: {
  entry: ErrorGroupActivityResponse
  projectId: number
  groupId: number
  canEdit: boolean
  canDelete: boolean
  onChanged: () => void
}) {
  const [editing, setEditing] = useState(false)
  const editor = useForm<CommentFormData>({
    resolver: zodResolver(commentFormSchema),
    defaultValues: { body: entry.body ?? '' },
  })
  const editIsBlank =
    useWatch({ control: editor.control, name: 'body' }).trim().length === 0
  const editError = editor.formState.errors.body?.message
  const [confirmingDelete, setConfirmingDelete] = useState(false)
  const path = {
    project_id: projectId,
    group_id: groupId,
    comment_id: entry.id,
  }

  const update = useMutation({
    ...updateErrorGroupCommentMutation(),
    meta: { errorTitle: 'Failed to change the comment' },
    onSuccess: () => {
      setEditing(false)
      onChanged()
    },
  })
  const remove = useMutation({
    ...deleteErrorGroupCommentMutation(),
    meta: { errorTitle: 'Failed to delete the comment' },
    onSuccess: onChanged,
    onSettled: () => setConfirmingDelete(false),
  })

  const actor = activityActor(entry)
  const time = (
    <span className="text-xs text-muted-foreground">
      <TimeAgo date={entry.created_at} />
      {entry.edited_at ? ' (edited)' : ''}
    </span>
  )

  if (entry.kind !== 'comment') {
    return (
      <li className="flex flex-wrap items-baseline gap-x-2 px-1 text-sm">
        {actor && <span className="font-medium">{actor}</span>}
        <span className="text-muted-foreground">{describeActivity(entry)}</span>
        {time}
      </li>
    )
  }

  return (
    <li>
      <Card>
        <CardContent className="space-y-2 p-3">
          <div className="flex items-center justify-between gap-2">
            <div className="flex items-baseline gap-2 text-sm">
              <span className="font-medium">{actor}</span>
              {time}
            </div>
            <div className="flex gap-1">
              {canEdit && !editing && (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-7 w-7"
                  aria-label="Edit comment"
                  onClick={() => {
                    editor.reset({ body: entry.body ?? '' })
                    setEditing(true)
                  }}
                >
                  <Pencil className="h-3.5 w-3.5" />
                </Button>
              )}
              {canDelete && (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-7 w-7"
                  aria-label="Delete comment"
                  disabled={remove.isPending}
                  onClick={() => setConfirmingDelete(true)}
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </Button>
              )}
            </div>
          </div>
          {editing ? (
            <form
              className="space-y-2"
              noValidate
              onSubmit={editor.handleSubmit(({ body }) =>
                update.mutate({ path, body: { body } })
              )}
            >
              <Textarea
                aria-label="Edit the comment"
                aria-invalid={editError ? true : undefined}
                maxLength={MAX_COMMENT_CHARS}
                rows={3}
                {...editor.register('body')}
              />
              {editError && (
                <p className="text-xs text-destructive">{editError}</p>
              )}
              <div className="flex justify-end gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => setEditing(false)}
                >
                  Cancel
                </Button>
                <Button
                  type="submit"
                  size="sm"
                  disabled={editIsBlank || update.isPending}
                >
                  Save
                </Button>
              </div>
            </form>
          ) : (
            <p className="whitespace-pre-wrap break-words text-sm">
              {entry.body}
            </p>
          )}
        </CardContent>
      </Card>
      <ConfirmDeleteDialog
        open={confirmingDelete}
        onOpenChange={setConfirmingDelete}
        resourceType="comment"
        resourceName={commentPreview(entry.body, actor)}
        isPending={remove.isPending}
        onConfirm={() => remove.mutate({ path })}
      />
    </li>
  )
}
