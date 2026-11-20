// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link } from 'react-router'
import {
  type QueryKey,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import { Download, Paperclip, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import {
  deleteErrorAttachmentMutation,
  getErrorAttachmentUsageOptions,
  getErrorAttachmentUsageQueryKey,
  listErrorEventAttachmentsQueryKey,
  listErrorEventAttachmentsOptions,
  listFeedbackAttachmentsOptions,
  listFeedbackAttachmentsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { AttachmentUsage, AttachmentView } from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { queryKeyPrefix } from '@/lib/refresh'
import { readFailureServerDetail } from '@/lib/read-failure'
import { formatBytesBinary } from '@/lib/server-monitoring'
import {
  ATTACHMENT_EXAMPLE,
  attachmentContentUrl,
  pollWhileEmpty,
  isInlineImage,
  STORAGE_SETUP_PATH,
} from './attachments'

interface AttachmentItemsProps {
  projectId: number
  /** "event" or "feedback", for the empty state. */
  ownerLabel: string
  items: AttachmentView[] | undefined
  isPending: boolean
  error: unknown
  usage: AttachmentUsage | undefined
  onDelete: (attachment: AttachmentView) => void
}

/** The onboarding state shown while blob storage is missing. */
function StorageMissing({ usage }: { usage: AttachmentUsage }) {
  return (
    <Alert>
      <AlertDescription className="space-y-2">
        <p>
          {usage.reason ??
            'No blob storage is set up, so attachments sent by SDKs are dropped.'}
        </p>
        <p className="text-muted-foreground">
          With it, a file an SDK sends with an error shows up here, for example:{' '}
          <code className="text-xs">{ATTACHMENT_EXAMPLE}</code>
        </p>
        <Link
          className="font-medium underline"
          to={usage.setup_path ?? STORAGE_SETUP_PATH}
        >
          Enable Blob storage
        </Link>
      </AlertDescription>
    </Alert>
  )
}

function AttachmentRow({
  projectId,
  attachment,
  onDelete,
}: {
  projectId: number
  attachment: AttachmentView
  onDelete: (attachment: AttachmentView) => void
}) {
  const url = attachmentContentUrl(projectId, attachment.id)
  return (
    <li className="flex flex-col gap-2 rounded-md border p-3 sm:flex-row sm:items-start">
      {isInlineImage(attachment.content_type) && (
        <a href={url} target="_blank" rel="noreferrer" className="shrink-0">
          <img
            src={url}
            alt={attachment.filename}
            loading="lazy"
            className="max-h-40 max-w-full rounded border sm:max-w-[12rem]"
          />
        </a>
      )}
      <div className="min-w-0 flex-1">
        <p className="text-sm font-medium [overflow-wrap:anywhere]">
          {attachment.filename}
        </p>
        <p className="text-xs text-muted-foreground">
          {formatBytesBinary(attachment.size_bytes)} · {attachment.content_type}
        </p>
      </div>
      <div className="flex gap-1">
        <Button variant="outline" size="sm" asChild>
          <a href={url} download={attachment.filename}>
            <Download className="h-4 w-4" />
            <span className="max-sm:sr-only">Download</span>
          </a>
        </Button>
        <Button
          variant="ghost"
          size="sm"
          aria-label={`Delete ${attachment.filename}`}
          onClick={() => onDelete(attachment)}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </li>
  )
}

/** The files sent with an event or feedback, or why there are none. */
export function AttachmentItems({
  projectId,
  ownerLabel,
  items,
  isPending,
  error,
  usage,
  onDelete,
}: AttachmentItemsProps) {
  const list = items ?? []
  return (
    <section className="space-y-3" aria-label="Attachments">
      <h2 className="flex items-center gap-2 text-base font-semibold">
        <Paperclip className="h-4 w-4" aria-hidden />
        Attachments
      </h2>
      {usage && !usage.configured && <StorageMissing usage={usage} />}
      {isPending ? (
        <div className="space-y-2">
          <Skeleton className="h-14 w-full" />
        </div>
      ) : error ? (
        <Alert variant="destructive">
          <AlertDescription>
            {readFailureServerDetail(error) ??
              'The attachments could not be loaded. Retry.'}
          </AlertDescription>
        </Alert>
      ) : list.length === 0 ? (
        usage?.configured !== false && (
          <p className="text-sm text-muted-foreground">
            No files were sent with this {ownerLabel}. An SDK attaches one with{' '}
            <code className="text-xs">{ATTACHMENT_EXAMPLE}</code>.
          </p>
        )
      ) : (
        <ul className="space-y-2">
          {list.map((attachment) => (
            <AttachmentRow
              key={attachment.id}
              projectId={projectId}
              attachment={attachment}
              onDelete={onDelete}
            />
          ))}
        </ul>
      )}
    </section>
  )
}

/** Deleting an attachment, with its confirmation. */
function useAttachmentDelete(projectId: number, listKeyPrefix: QueryKey) {
  const queryClient = useQueryClient()
  const [deleting, setDeleting] = useState<AttachmentView | null>(null)
  const remove = useMutation({
    ...deleteErrorAttachmentMutation(),
    meta: { errorTitle: 'Failed to delete the attachment' },
    onSuccess: () => {
      toast.success('Attachment deleted')
      void queryClient.invalidateQueries({ queryKey: listKeyPrefix })
      void queryClient.invalidateQueries({
        queryKey: getErrorAttachmentUsageQueryKey({
          path: { project_id: projectId },
        }),
      })
    },
    onSettled: () => setDeleting(null),
  })
  const dialog = (
    <ConfirmDeleteDialog
      open={deleting !== null}
      onOpenChange={(open) => {
        if (!open) setDeleting(null)
      }}
      resourceType="attachment"
      resourceName={deleting?.filename ?? ''}
      consequences="The file is deleted from blob storage and its space returns to the project's quota. The event or feedback it came with is kept."
      isPending={remove.isPending}
      onConfirm={() => {
        if (deleting) {
          remove.mutate({
            path: { project_id: projectId, attachment_id: deleting.id },
          })
        }
      }}
    />
  )
  return { onDelete: setDeleting, dialog }
}

function useUsage(projectId: number) {
  return useQuery({
    ...getErrorAttachmentUsageOptions({ path: { project_id: projectId } }),
    staleTime: 60_000,
  })
}

/** The attachments of an error event (ADR 111, D4). */
export function EventAttachments({
  projectId,
  groupId,
  eventId,
}: {
  projectId: number
  groupId: number
  eventId: number
}) {
  const path = { project_id: projectId, group_id: groupId, event_id: eventId }
  const usage = useUsage(projectId)
  const list = useQuery({
    ...listErrorEventAttachmentsOptions({ path }),
    refetchInterval: pollWhileEmpty(usage.data?.configured),
  })
  const { onDelete, dialog } = useAttachmentDelete(
    projectId,
    queryKeyPrefix(listErrorEventAttachmentsQueryKey, { path })
  )
  return (
    <>
      <AttachmentItems
        projectId={projectId}
        ownerLabel="event"
        items={list.data?.data}
        isPending={list.isPending}
        error={list.error}
        usage={usage.data}
        onDelete={onDelete}
      />
      {dialog}
    </>
  )
}

/** The attachments of a feedback, such as its screenshot (ADR 111, D4). */
export function FeedbackAttachments({
  projectId,
  feedbackId,
}: {
  projectId: number
  feedbackId: number
}) {
  const path = { project_id: projectId, feedback_id: feedbackId }
  const usage = useUsage(projectId)
  const list = useQuery({
    ...listFeedbackAttachmentsOptions({ path }),
    refetchInterval: pollWhileEmpty(usage.data?.configured),
  })
  const { onDelete, dialog } = useAttachmentDelete(
    projectId,
    queryKeyPrefix(listFeedbackAttachmentsQueryKey, { path })
  )
  return (
    <>
      <AttachmentItems
        projectId={projectId}
        ownerLabel="feedback"
        items={list.data?.data}
        isPending={list.isPending}
        error={list.error}
        usage={usage.data}
        onDelete={onDelete}
      />
      {dialog}
    </>
  )
}
