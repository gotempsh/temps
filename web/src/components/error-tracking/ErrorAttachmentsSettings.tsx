// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { zodResolver } from '@hookform/resolvers/zod'
import { useForm } from 'react-hook-form'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  getErrorAttachmentUsageOptions,
  getErrorAttachmentUsageQueryKey,
  setErrorAttachmentQuotaMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { AttachmentUsage } from '@/api/client/types.gen'
import { Callout, SaveRow } from '@temps-sdk/ds'
import { PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Progress } from '@/components/ui/progress'
import { Skeleton } from '@/components/ui/skeleton'
import { readFailureServerDetail } from '@/lib/read-failure'
import { formatBytesBinary } from '@/lib/server-monitoring'
import {
  ATTACHMENT_EXAMPLE,
  bytesToMib,
  isWholeMib,
  MAX_QUOTA_MIB,
  mibToBytes,
  quotaFormSchema,
  quotaShare,
  storeSavedUsage,
  STORAGE_SETUP_PATH,
  type QuotaFormData,
} from './attachments'

/** What the attachments of a project use, and what they are limited to. */
export function AttachmentUsageSummary({ usage }: { usage: AttachmentUsage }) {
  const share = quotaShare(usage.used_bytes, usage.quota_bytes)
  return (
    <div className="space-y-3">
      {!usage.configured && (
        <Alert>
          <AlertDescription className="space-y-2">
            <p>{usage.reason}</p>
            <p className="text-muted-foreground">
              With it, files SDKs send with errors and feedback are kept, for
              example: <code className="text-xs">{ATTACHMENT_EXAMPLE}</code>
            </p>
            <Link
              className="font-medium underline"
              to={usage.setup_path ?? STORAGE_SETUP_PATH}
            >
              Enable Blob storage
            </Link>
          </AlertDescription>
        </Alert>
      )}
      <div className="space-y-1.5">
        <div className="flex flex-wrap justify-between gap-x-4 text-sm">
          <span>
            {formatBytesBinary(usage.used_bytes)} of{' '}
            {formatBytesBinary(usage.quota_bytes)} used
          </span>
          <span className="text-muted-foreground">
            {usage.files.toLocaleString()}{' '}
            {usage.files === 1 ? 'file' : 'files'}
          </span>
        </div>
        <Progress
          value={share * 100}
          aria-label="Attachment quota used"
          className="h-2"
        />
      </div>
      <p className="text-xs text-muted-foreground">
        Up to {formatBytesBinary(usage.max_attachment_bytes)} per file and{' '}
        {formatBytesBinary(usage.max_event_attachment_bytes)} or{' '}
        {usage.max_attachments_per_event} files per event. Attachments are
        deleted with the event or feedback they came with.
      </p>
      {(usage.dropped_over_quota > 0 || usage.dropped_queue_full > 0) && (
        <p className="text-xs text-amber-700 dark:text-amber-400">
          Since the server started, across projects:{' '}
          {usage.dropped_over_quota.toLocaleString()} dropped over quota,{' '}
          {usage.dropped_queue_full.toLocaleString()} dropped while the upload
          queue was full.
        </p>
      )}
    </div>
  )
}

export function QuotaForm({
  projectId,
  usage,
  usageReadAt,
  refreshing = false,
}: {
  projectId: number
  usage: AttachmentUsage
  /** When `usage` was last read from the server (React Query `dataUpdatedAt`). */
  usageReadAt: number
  /** The saved quota is being read again. */
  refreshing?: boolean
}) {
  const queryClient = useQueryClient()
  const form = useForm<QuotaFormData>({
    resolver: zodResolver(quotaFormSchema),
    defaultValues: { quota_mib: bytesToMib(usage.quota_bytes) },
  })
  const { reset } = form
  const { isDirty } = form.formState
  // When the last save failed. Its outcome is unknown (the answer may have
  // been lost after the commit), so no new save until the saved quota has
  // been read again after it.
  const [failedAt, setFailedAt] = useState<number | null>(null)
  const awaitingRead = failedAt !== null && usageReadAt <= failedAt
  // A refresh brings the saved quota in, but never over an unsaved edit.
  useEffect(() => {
    if (!isDirty) reset({ quota_mib: bytesToMib(usage.quota_bytes) })
  }, [reset, isDirty, usage.quota_bytes])
  const save = useMutation({
    ...setErrorAttachmentQuotaMutation(),
    meta: { errorTitle: 'Failed to change the attachment quota' },
    onMutate: () => setFailedAt(null),
    onSuccess: async (next) => {
      reset({ quota_mib: bytesToMib(next.quota_bytes) })
      toast.success(
        `Attachment quota set to ${formatBytesBinary(next.quota_bytes)}`
      )
      await storeSavedUsage(queryClient, projectId, next)
    },
    // The edit stays in the form; the summary shows what the server holds.
    onError: () => {
      setFailedAt(Date.now())
      void queryClient.invalidateQueries({
        queryKey: getErrorAttachmentUsageQueryKey({
          path: { project_id: projectId },
        }),
      })
    },
  })
  const error = form.formState.errors.quota_mib?.message
  const exact = isWholeMib(usage.quota_bytes)
  const saveError = save.isError
    ? (readFailureServerDetail(save.error) ??
      'The server did not confirm the change.')
    : null
  let hint: string | undefined
  if (awaitingRead) {
    hint = refreshing
      ? 'Reading the saved quota again before another save…'
      : 'Read the saved quota again (Retry above) before saving again.'
  } else if (!exact && !isDirty) {
    hint = `The current quota, ${formatBytesBinary(usage.quota_bytes)}, is not a whole number of MiB; enter a new value to replace it.`
  }
  return (
    <form
      noValidate
      className="space-y-4"
      onSubmit={form.handleSubmit(({ quota_mib }) =>
        save.mutate({
          path: { project_id: projectId },
          body: { quota_bytes: mibToBytes(quota_mib) },
        })
      )}
    >
      <div className="space-y-1.5">
        <Label htmlFor="attachment-quota">Quota (MiB)</Label>
        <Input
          id="attachment-quota"
          type="number"
          inputMode="numeric"
          min={0}
          max={MAX_QUOTA_MIB}
          className="w-full sm:w-40"
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? 'attachment-quota-error' : undefined}
          disabled={save.isPending}
          {...form.register('quota_mib', { valueAsNumber: true })}
        />
        {error && (
          <p id="attachment-quota-error" className="text-xs text-destructive">
            {error}
          </p>
        )}
      </div>
      {saveError && (
        <Callout
          tone="error"
          title="The attachment quota may not have been saved"
        >
          {saveError} The summary above shows the quota the server holds; your
          value is still in the field, so you can save it again.
        </Callout>
      )}
      <SaveRow
        dirty={isDirty}
        saving={save.isPending}
        disabled={refreshing || awaitingRead}
        label={saveError ? 'Save again' : 'Save quota'}
        hint={hint}
      />
    </form>
  )
}

/**
 * The usage and quota form, or why they cannot be shown. A failed refresh
 * keeps the form mounted, with any unsaved edit, under its error: the
 * error sits in its own slot so the form never changes position and
 * remounts.
 */
export function AttachmentsSettingsBody({
  projectId,
  usage,
  usageReadAt,
  isPending,
  error,
  isRetrying,
  onRetry,
}: {
  projectId: number
  usage: AttachmentUsage | undefined
  /** When `usage` was last read from the server. */
  usageReadAt: number
  isPending: boolean
  error: unknown
  isRetrying: boolean
  onRetry: () => void
}) {
  if (isPending) {
    return (
      <div className="space-y-3">
        <Skeleton className="h-5 w-64" />
        <Skeleton className="h-2 w-full" />
        <Skeleton className="h-9 w-56" />
      </div>
    )
  }
  const failure = error ? (
    <Alert variant="destructive">
      <AlertDescription className="space-y-2">
        <p>
          {readFailureServerDetail(error) ??
            (usage
              ? 'Attachment usage could not be refreshed; the values below may be out of date.'
              : 'Attachment usage could not be loaded.')}
        </p>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={isRetrying}
          onClick={onRetry}
        >
          {isRetrying ? 'Retrying…' : 'Retry'}
        </Button>
      </AlertDescription>
    </Alert>
  ) : null
  return (
    <>
      {failure}
      {usage && <AttachmentUsageSummary usage={usage} />}
      {usage && (
        // Keyed by project: an unsaved edit never carries over to another
        // project's quota.
        <QuotaForm
          key={projectId}
          projectId={projectId}
          usage={usage}
          usageReadAt={usageReadAt}
          refreshing={isRetrying}
        />
      )}
    </>
  )
}

/**
 * Files SDKs send with errors and feedback (ADR 111, D4): whether they are
 * kept, how much of the project's quota they use, and the quota itself.
 */
export function ErrorAttachmentsSettings({ projectId }: { projectId: number }) {
  const usage = useQuery(
    getErrorAttachmentUsageOptions({ path: { project_id: projectId } })
  )
  return (
    <div className="space-y-4">
      <PageHeader title="Attachments" />
      <Card>
        <CardContent className="space-y-6 pt-6">
          <AttachmentsSettingsBody
            projectId={projectId}
            usage={usage.data}
            usageReadAt={usage.dataUpdatedAt}
            isPending={usage.isPending}
            error={usage.error}
            isRetrying={usage.isFetching}
            onRetry={() => void usage.refetch()}
          />
        </CardContent>
      </Card>
    </div>
  )
}
