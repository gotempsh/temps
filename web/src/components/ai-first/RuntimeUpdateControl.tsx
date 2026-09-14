// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation } from '@tanstack/react-query'
import { ArrowUpCircle, Loader2 } from 'lucide-react'
import { useState } from 'react'
import {
  controlApplicationWorkspace,
  type ApplicationWorkspaceResponse,
} from '@/api/client'
import { Button } from '@/components/ui/button'
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogCancel,
} from '@/components/ui/alert-dialog'
import { problemDetail } from './problem-detail'

export const RUNTIME_UPDATE_WARNING =
  'Workspace files and saved settings are preserved. Running processes will stop and in-memory agent sessions will be renewed. Finish or stop all active threads before updating.'

export function RuntimeUpdateControl({
  applicationPublicId,
  workspace,
  runtime,
  disabled = false,
  onUpdated,
}: {
  applicationPublicId: string
  workspace: ApplicationWorkspaceResponse
  runtime?: string
  disabled?: boolean
  onUpdated: (workspace: ApplicationWorkspaceResponse) => void
}) {
  const [open, setOpen] = useState(false)
  const update = useMutation({
    mutationFn: async () => {
      const { data } = await controlApplicationWorkspace({
        path: { application_public_id: applicationPublicId },
        body: { action: 'update_runtime', confirm: true, runtime },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (next) => {
      onUpdated(next)
      setOpen(false)
    },
  })
  const required = workspace.runtime_compatible === false
  return (
    <section
      className="space-y-3 rounded-xl border border-border p-3"
      aria-label="Agent runtime update"
    >
      <div>
        <p className="text-xs font-medium">
          {required ? 'Runtime update required' : 'Agent runtime'}
        </p>
        <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
          {required
            ? 'This runtime cannot run chat sessions with this version of Temps. Update it to continue; restarting keeps the same image.'
            : 'Update the sandbox to the compatible runtime selected by this Temps release.'}
        </p>
      </div>
      {runtime && runtime !== workspace.runtime && (
        <p className="text-xs font-medium">
          Selected flavor: {runtime}. Confirm the runtime update to apply this
          change.
        </p>
      )}
      {workspace.runtime_update_image && runtime === workspace.runtime && (
        <p className="break-all font-mono text-[10px] text-muted-foreground">
          {workspace.runtime_update_image}
        </p>
      )}
      {workspace.runtime_update_error && (
        <p className="text-xs text-destructive">
          {workspace.runtime_update_error}
        </p>
      )}
      <Button
        size="sm"
        variant={required ? 'default' : 'outline'}
        className="w-full"
        disabled={
          disabled || update.isPending || !workspace.runtime_update_available
        }
        onClick={() => {
          update.reset()
          setOpen(true)
        }}
      >
        <ArrowUpCircle className="mr-2 size-3.5" /> Update runtime
      </Button>
      {!workspace.runtime_update_available && (
        <p className="text-xs text-muted-foreground">
          No runtime update is available for this sandbox. Check the runtime
          status or ask your instance administrator to configure a compatible
          image.
        </p>
      )}
      <AlertDialog
        open={open}
        onOpenChange={(next) => {
          if (!update.isPending) setOpen(next)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Update sandbox runtime?</AlertDialogTitle>
            <AlertDialogDescription>
              {RUNTIME_UPDATE_WARNING}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <p className="text-sm text-muted-foreground">
            Temps validates the replacement before stopping your sandbox. If
            replacement fails, it attempts to restore the previous image. An
            update does not automatically restart your app server.
          </p>
          {update.isError && (
            <p role="alert" className="text-sm text-destructive">
              {problemDetail(
                update.error,
                'The runtime update failed. Your workspace files have not been deleted.'
              )}
            </p>
          )}
          {update.isPending && (
            <p role="status" className="text-sm text-muted-foreground">
              Checking and updating runtime… This may take a few minutes.
            </p>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel disabled={update.isPending}>
              Cancel
            </AlertDialogCancel>
            <Button disabled={update.isPending} onClick={() => update.mutate()}>
              {update.isPending && (
                <Loader2 className="mr-2 size-4 animate-spin" />
              )}
              Confirm runtime update
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </section>
  )
}
