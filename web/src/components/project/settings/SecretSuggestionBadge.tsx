// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SecretSuggestionResponse } from '@/api/client'
import {
  dismissEnvironmentVariableSecretSuggestionMutation,
  updateEnvironmentVariableMutation,
} from '@/api/client/@tanstack/react-query.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { useMutation } from '@tanstack/react-query'
import { ShieldAlert } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'

export interface SecretSuggestionBadgeProps {
  projectId: number
  variableId: number
  variableKey: string
  /** Absent when there is nothing to flag; the badge then renders nothing. */
  suggestion?: SecretSuggestionResponse | null
  /** Current bindings, sent back unchanged when converting. */
  environmentIds: number[]
  includeInPreview: boolean
  /** Called once the variable is a secret, to reload and offer a redeploy. */
  onConverted: () => void
  /** Called once the dismissal is saved, to reload the list. */
  onDismissed: () => void
}

/**
 * Flags a regular variable whose value looks like a credential. The server
 * decides that (`secret_suggestion` on the variable); this only explains it
 * and offers the two answers: store it as a secret, or say it is not one.
 */
export function SecretSuggestionBadge({
  projectId,
  variableId,
  variableKey,
  suggestion,
  environmentIds,
  includeInPreview,
  onConverted,
  onDismissed,
}: SecretSuggestionBadgeProps) {
  const [open, setOpen] = useState(false)
  const [confirming, setConfirming] = useState(false)
  // Converts without a value: the API keeps the stored ciphertext, so the
  // plaintext is never revealed just to change the flag.
  const convert = useMutation({
    ...updateEnvironmentVariableMutation(),
    meta: { errorTitle: `Failed to convert ${variableKey} to a secret` },
    onSuccess: () => {
      setConfirming(false)
      onConverted()
    },
  })
  const dismiss = useMutation({
    ...dismissEnvironmentVariableSecretSuggestionMutation(),
    meta: { errorTitle: `Failed to dismiss the suggestion for ${variableKey}` },
    onSuccess: () => {
      setOpen(false)
      toast.success(`${variableKey} will no longer be flagged`, {
        description: 'It is flagged again if its value changes.',
      })
      onDismissed()
    },
  })

  if (!suggestion) return null

  return (
    <>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <button
            type="button"
            className="rounded-full focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            aria-label={`${variableKey} looks like a credential`}
          >
            <Badge variant="warning-subtle" className="gap-1 whitespace-nowrap">
              <ShieldAlert className="h-3 w-3" />
              Store as secret?
            </Badge>
          </button>
        </PopoverTrigger>
        <PopoverContent className="w-80 space-y-3 text-sm">
          <p className="font-medium">{suggestion.message}.</p>
          <p className="text-muted-foreground">
            Its value can be revealed by anyone who can edit this project&apos;s
            variables. As a secret it becomes write-only and is never shown
            again.
          </p>
          <div className="flex flex-col gap-2 sm:flex-row sm:justify-end">
            <Button
              variant="ghost"
              size="sm"
              disabled={dismiss.isPending}
              onClick={() =>
                dismiss.mutate({
                  path: { project_id: projectId, var_id: variableId },
                })
              }
            >
              It&apos;s not a secret
            </Button>
            <Button
              size="sm"
              onClick={() => {
                setOpen(false)
                setConfirming(true)
              }}
            >
              Store as secret
            </Button>
          </div>
        </PopoverContent>
      </Popover>
      <AlertDialog
        open={confirming}
        onOpenChange={(next) => {
          if (!convert.isPending) setConfirming(next)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Convert {variableKey} to a secret?
            </AlertDialogTitle>
            <AlertDialogDescription className="space-y-2">
              <span className="block">
                The current value is kept, but it becomes write-only: nobody can
                reveal, copy or export it again. To make it a regular variable
                again you must delete it and create it anew.
              </span>
              <span className="block">
                If the value may already have been seen, rotate it where it was
                issued and then update the secret with the new value.
              </span>
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={convert.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              disabled={convert.isPending}
              onClick={(event) => {
                // Keep the dialog open until the request settles.
                event.preventDefault()
                convert.mutate({
                  path: { project_id: projectId, var_id: variableId },
                  body: {
                    key: variableKey,
                    environment_ids: environmentIds,
                    include_in_preview: includeInPreview,
                    is_secret: true,
                  },
                })
              }}
            >
              {convert.isPending ? 'Converting…' : 'Convert to secret'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  )
}
