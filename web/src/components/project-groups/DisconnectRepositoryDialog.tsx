// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation } from '@tanstack/react-query'
import { toast } from 'sonner'
import { clearGroupRepositoryMutation } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectGroupResponse } from '@/api/client'
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
import { getErrorMessage } from '@/utils/errorHandling'
import { useInvalidateGroups } from './useProjectGroups'

interface DisconnectRepositoryDialogProps {
  group: ProjectGroupResponse
  open: boolean
  onOpenChange: (open: boolean) => void
}

/** Confirms removing the project's repository; services keep theirs. */
export function DisconnectRepositoryDialog({
  group,
  open,
  onOpenChange,
}: DisconnectRepositoryDialogProps) {
  const invalidate = useInvalidateGroups()
  const clear = useMutation({ ...clearGroupRepositoryMutation() })
  const repo = group.repository
  const repoLabel = repo
    ? `${repo.repo_owner}/${repo.repo_name}`
    : 'the repository'

  const disconnect = async () => {
    try {
      await clear.mutateAsync({ path: { group_id: group.id } })
      await invalidate()
      toast.success(
        `${group.name} no longer has a repository; its services keep theirs`
      )
    } catch (err) {
      toast.error(getErrorMessage(err, 'Could not disconnect the repository'))
    } finally {
      onOpenChange(false)
    }
  }

  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Disconnect {repoLabel}?</AlertDialogTitle>
          <AlertDialogDescription>
            Services keep deploying from their current repository, branch and
            directory; they just stop following the project, so changing the
            branch here no longer changes theirs. Nothing is redeployed.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            onClick={() => void disconnect()}
            disabled={clear.isPending}
          >
            Disconnect
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
