// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  listErrorGroupFingerprintsOptions,
  listErrorGroupFingerprintsQueryKey,
  getErrorGroupQueryKey,
  splitErrorGroupMutation,
  unmergeErrorGroupMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Merge, Split } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'

interface GroupMergeActionsProps {
  projectId: number
  projectSlug: string
  groupId: number
  /** Set when this group was merged into another one. */
  mergedIntoGroupId?: number | null
}

/**
 * Merge state of one error group (ADR 074, D1.2): where it was merged, what
 * was merged into it, and the two ways back out — unmerge for a merged group,
 * split for a group whose fingerprints turn out to be two different bugs.
 */
export function GroupMergeActions({
  projectId,
  projectSlug,
  groupId,
  mergedIntoGroupId,
}: GroupMergeActionsProps) {
  const queryClient = useQueryClient()
  const [splitOpen, setSplitOpen] = useState(false)
  const [chosen, setChosen] = useState<string[]>([])

  const { data: fingerprints, isPending } = useQuery({
    ...listErrorGroupFingerprintsOptions({
      path: { project_id: projectId, group_id: groupId },
    }),
  })

  const invalidate = () => {
    queryClient.invalidateQueries({
      queryKey: getErrorGroupQueryKey({
        path: { project_id: projectId, group_id: groupId },
      }),
    })
    queryClient.invalidateQueries({
      queryKey: listErrorGroupFingerprintsQueryKey({
        path: { project_id: projectId, group_id: groupId },
      }),
    })
  }

  const unmergeMutation = useMutation({
    ...unmergeErrorGroupMutation(),
    meta: { errorTitle: 'Failed to take this group out of its merge' },
    onSuccess: (response) => {
      toast.success('Error group unmerged', {
        description: `Its future events no longer land in #${response.target_group_id}.`,
      })
      invalidate()
    },
  })

  const splitMutation = useMutation({
    ...splitErrorGroupMutation(),
    meta: { errorTitle: 'Failed to split the selected fingerprints' },
    onSuccess: (response) => {
      setSplitOpen(false)
      setChosen([])
      toast.success('Fingerprints split out', {
        description: `Their future events land in #${response.new_group_id}.`,
      })
      invalidate()
    },
  })

  const splittable = (fingerprints?.fingerprints ?? []).filter(
    (entry) => entry.merged_from_group_id == null
  )
  const mergedIn = (fingerprints?.fingerprints ?? []).filter(
    (entry) => entry.merged_from_group_id != null
  )
  const mergedGroupIds = fingerprints?.merged_group_ids ?? []

  if (mergedIntoGroupId != null) {
    return (
      <Alert>
        <Merge className="h-4 w-4" />
        <AlertDescription className="flex flex-col gap-2 text-sm sm:flex-row sm:items-center sm:justify-between">
          <span>
            Merged into{' '}
            <Link
              className="font-medium underline"
              to={`/projects/${projectSlug}/errors/${mergedIntoGroupId}`}
            >
              #{mergedIntoGroupId}
            </Link>
            . New events of this error are counted there; the events below are
            the ones it had before the merge.
          </span>
          <Button
            size="sm"
            variant="outline"
            disabled={unmergeMutation.isPending}
            onClick={() =>
              unmergeMutation.mutate({
                path: { project_id: projectId, group_id: groupId },
              })
            }
          >
            Unmerge
          </Button>
        </AlertDescription>
      </Alert>
    )
  }

  if (isPending) {
    return <Skeleton className="h-9 w-40" />
  }

  return (
    <div className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
      {mergedGroupIds.length > 0 && (
        <span>
          {mergedGroupIds.length === 1
            ? '1 group merged into this one'
            : `${mergedGroupIds.length} groups merged into this one`}
          {': '}
          {mergedGroupIds.map((id, index) => (
            <span key={id}>
              {index > 0 && ', '}
              <Link
                className="underline"
                to={`/projects/${projectSlug}/errors/${id}`}
              >
                #{id}
              </Link>
            </span>
          ))}
          {mergedIn.length > 0 && ` (${mergedIn.length} fingerprints)`}
        </span>
      )}
      <Dialog open={splitOpen} onOpenChange={setSplitOpen}>
        <DialogTrigger asChild>
          <Button size="sm" variant="outline" disabled={splittable.length < 2}>
            <Split className="h-3.5 w-3.5" />
            Split
            {splittable.length > 1 ? ` (${splittable.length})` : ''}
          </Button>
        </DialogTrigger>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Split fingerprints into a new group</DialogTitle>
          </DialogHeader>
          <p className="text-sm text-muted-foreground">
            Events already stored stay in this group. The fingerprints you pick
            send their future events to a new group instead. Keep at least one
            here.
          </p>
          <div className="max-h-64 space-y-2 overflow-y-auto">
            {splittable.map((entry) => (
              <label
                key={entry.fingerprint_hash}
                className="flex items-center gap-2 font-mono text-xs [overflow-wrap:anywhere]"
              >
                <Checkbox
                  checked={chosen.includes(entry.fingerprint_hash)}
                  onCheckedChange={(checked) =>
                    setChosen((current) =>
                      checked === true
                        ? [...current, entry.fingerprint_hash]
                        : current.filter(
                            (hash) => hash !== entry.fingerprint_hash
                          )
                    )
                  }
                />
                {entry.fingerprint_hash.slice(0, 16)}…
              </label>
            ))}
          </div>
          <DialogFooter>
            <Button
              disabled={
                chosen.length === 0 ||
                chosen.length === splittable.length ||
                splitMutation.isPending
              }
              onClick={() =>
                splitMutation.mutate({
                  path: { project_id: projectId, group_id: groupId },
                  body: { fingerprints: chosen },
                })
              }
            >
              Split {chosen.length > 0 ? chosen.length : ''}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
