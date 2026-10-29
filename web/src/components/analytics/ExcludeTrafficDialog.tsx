// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Confirm-and-create dialog behind every inline "Exclude from analytics"
 * action (a referrer on the Referrers card, an IP or network on a visitor).
 * It creates one traffic filter and points at the settings page where the
 * filter can be reviewed and removed.
 */

import type { TrafficFilterKind } from '@/api/client/types.gen'
import {
  createTrafficFilterMutation,
  getFilteredTrafficQueryKey,
  listTrafficFiltersQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  TRAFFIC_FILTER_KIND_COPY,
  formatTrafficFilterValue,
} from './traffic-filters-shared'

export interface ExcludeTrafficTarget {
  kind: TrafficFilterKind
  value: string
  /** Extra context for the confirmation, e.g. the network's organisation. */
  detail?: string
}

interface ExcludeTrafficDialogProps {
  projectId: number
  projectSlug: string
  /** What to exclude; the dialog is open while this is set. */
  target: ExcludeTrafficTarget | null
  onClose: () => void
}

export function ExcludeTrafficDialog({
  projectId,
  projectSlug,
  target,
  onClose,
}: ExcludeTrafficDialogProps) {
  const queryClient = useQueryClient()
  const [note, setNote] = useState('')

  const create = useMutation({
    ...createTrafficFilterMutation(),
    meta: { errorTitle: 'Could not add the traffic filter' },
    onSuccess: (filter) => {
      void queryClient.invalidateQueries({
        queryKey: listTrafficFiltersQueryKey({
          path: { project_id: projectId },
        }),
      })
      void queryClient.invalidateQueries({
        queryKey: getFilteredTrafficQueryKey({
          path: { project_id: projectId },
        }),
      })
      toast.success(
        `${formatTrafficFilterValue(filter.kind, filter.value)} is excluded from analytics`,
        {
          description:
            'Applies to visits from now on. Manage it under Analytics → Traffic filters.',
        }
      )
      setNote('')
      onClose()
    },
  })

  const kindNoun = target ? TRAFFIC_FILTER_KIND_COPY[target.kind].noun : ''
  const shownValue = target
    ? formatTrafficFilterValue(target.kind, target.value)
    : ''

  return (
    <AlertDialog
      open={target !== null}
      onOpenChange={(open) => {
        if (!open && !create.isPending) {
          setNote('')
          onClose()
        }
      }}
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Exclude this {kindNoun}?</AlertDialogTitle>
          <AlertDialogDescription asChild>
            <div className="space-y-2">
              <p>
                Visits matching{' '}
                <span className="font-mono text-foreground">{shownValue}</span>
                {target?.detail ? ` (${target.detail})` : ''} will be left out
                of every analytics report for this project, from now on.
              </p>
              <p>
                Visits already recorded are not changed. You can remove the
                filter at any time under{' '}
                <Link
                  className="underline underline-offset-2"
                  to={`/projects/${projectSlug}/analytics/settings`}
                >
                  Traffic filters
                </Link>
                .
              </p>
            </div>
          </AlertDialogDescription>
        </AlertDialogHeader>
        <div className="space-y-2">
          <Label htmlFor="exclude-traffic-note">Note (optional)</Label>
          <Input
            id="exclude-traffic-note"
            placeholder="Why this is excluded, for whoever reviews it later"
            value={note}
            maxLength={500}
            onChange={(event) => setNote(event.target.value)}
          />
        </div>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={create.isPending}>
            Cancel
          </AlertDialogCancel>
          <Button
            variant="destructive"
            disabled={!target || create.isPending}
            onClick={() => {
              if (!target) return
              create.mutate({
                path: { project_id: projectId },
                body: {
                  kind: target.kind,
                  value: target.value,
                  note: note.trim() || null,
                },
              })
            }}
          >
            {create.isPending && (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            )}
            Exclude from analytics
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
