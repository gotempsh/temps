// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SecretSuggestionResponse } from '@/api/client'
import { dismissEnvironmentVariableSecretSuggestionMutation } from '@/api/client/@tanstack/react-query.gen'
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
  /** Opens the edit dialog with "Convert to secret" already switched on. */
  onStoreAsSecret: () => void
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
  onStoreAsSecret,
  onDismissed,
}: SecretSuggestionBadgeProps) {
  const [open, setOpen] = useState(false)
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
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          className="rounded-full focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          aria-label={`${variableKey} looks like a credential`}
        >
          <Badge variant="warning-subtle" className="gap-1">
            <ShieldAlert className="h-3 w-3" />
            Store as secret?
          </Badge>
        </button>
      </PopoverTrigger>
      <PopoverContent className="w-80 space-y-3 text-sm">
        <p className="font-medium">{suggestion.message}.</p>
        <p className="text-muted-foreground">
          Its value can be revealed by anyone who can edit this project&apos;s
          variables. Rotate it where it was issued, then save the new value as a
          secret: secrets are write-only and never shown again.
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
              onStoreAsSecret()
            }}
          >
            Store as secret
          </Button>
        </div>
      </PopoverContent>
    </Popover>
  )
}
