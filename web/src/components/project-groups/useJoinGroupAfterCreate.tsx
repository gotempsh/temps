// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Loader2, RefreshCw } from 'lucide-react'
import { getProjectGroupOptions } from '@/api/client/@tanstack/react-query.gen'
import { addProjectToGroup } from '@/api/client/sdk.gen'
import type { ProjectGroupResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { groupHref } from '@/lib/terminology'
import { getErrorMessage } from '@/utils/errorHandling'
import { useInvalidateGroups } from './useProjectGroups'

/** Search parameter of `/projects/new` naming the project to join. */
export const TARGET_GROUP_PARAM = 'group'

/** Where "New service" in a project leads. */
export function newServiceInGroupHref(groupId: number): string {
  return `/projects/new?${TARGET_GROUP_PARAM}=${groupId}`
}

export function parseTargetGroupId(raw: string | null): number | null {
  if (!raw || !/^\d+$/.test(raw)) return null
  const id = Number(raw)
  return Number.isSafeInteger(id) && id > 0 ? id : null
}

/** A created service, as much of it as the follow-up needs. */
export interface CreatedService {
  id: number
  slug: string
  name: string
}

interface JoinFailure {
  service: CreatedService
  message: string
}

/**
 * The project (group) a new service should join, from `?group=<id>`. Shared
 * by the page shell (to say where the service goes) and every creation flow.
 */
export function useTargetGroup() {
  const [searchParams] = useSearchParams()
  const groupId = parseTargetGroupId(searchParams.get(TARGET_GROUP_PARAM))
  const query = useQuery({
    ...getProjectGroupOptions({ path: { group_id: groupId ?? 0 } }),
    enabled: groupId !== null,
    retry: false,
  })
  return { groupId, group: query.data ?? null, query }
}

/**
 * After a creation flow makes a service: with `?group=<id>`, adds it to that
 * project and opens the project; otherwise goes where the flow always went.
 * A failed add never passes silently: `joinGroupDialog` (render it in the
 * flow) says so, with Retry and a link to the service that was created.
 */
export function useJoinGroupAfterCreate() {
  const navigate = useNavigate()
  const invalidate = useInvalidateGroups()
  const { groupId, group } = useTargetGroup()
  const [failure, setFailure] = useState<JoinFailure | null>(null)
  const [retrying, setRetrying] = useState(false)

  const join = useCallback(
    async (service: CreatedService): Promise<boolean> => {
      if (groupId === null) return true
      try {
        const { data } = await addProjectToGroup({
          path: { group_id: groupId, project_id: service.id },
          throwOnError: true,
        })
        await invalidate()
        toast.success(`${service.name} added to ${data.name}`)
        setFailure(null)
        navigate(groupHref(data.slug))
        return true
      } catch (err) {
        await invalidate()
        setFailure({
          service,
          message: getErrorMessage(
            err,
            'The service could not be added to the project.'
          ),
        })
        return false
      }
    },
    [groupId, invalidate, navigate]
  )

  /**
   * Finish a creation: join the target project when there is one, otherwise
   * navigate to `fallbackHref`. Resolves false when joining failed and the
   * dialog is showing; the caller stays where it is.
   */
  const finish = useCallback(
    async (service: CreatedService, fallbackHref: string): Promise<boolean> => {
      if (groupId === null) {
        navigate(fallbackHref)
        return true
      }
      return join(service)
    },
    [groupId, join, navigate]
  )

  const retry = async () => {
    if (!failure) return
    setRetrying(true)
    try {
      await join(failure.service)
    } finally {
      setRetrying(false)
    }
  }

  const joinGroupDialog = (
    <JoinGroupFailedDialog
      failure={failure}
      group={group}
      retrying={retrying}
      onRetry={() => void retry()}
      onClose={() => setFailure(null)}
    />
  )

  return { groupId, group, finish, joinGroupDialog }
}

function JoinGroupFailedDialog({
  failure,
  group,
  retrying,
  onRetry,
  onClose,
}: {
  failure: JoinFailure | null
  group: ProjectGroupResponse | null
  retrying: boolean
  onRetry: () => void
  onClose: () => void
}) {
  const groupName = group?.name ?? 'the project'
  return (
    <Dialog open={failure !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>
            {failure?.service.name ?? 'The service'} was created, but not added
            to {groupName}
          </DialogTitle>
          <DialogDescription>
            The service exists and works on its own. Retry adding it, or add it
            later from the project&apos;s Add services menu.
          </DialogDescription>
        </DialogHeader>
        {failure && (
          <p role="alert" className="text-sm text-destructive">
            {failure.message}
          </p>
        )}
        <DialogFooter className="gap-2 sm:gap-0">
          {failure && (
            <Button variant="outline" asChild>
              <Link to={`/projects/${failure.service.slug}`} onClick={onClose}>
                Open {failure.service.name}
              </Link>
            </Button>
          )}
          <Button onClick={onRetry} disabled={retrying}>
            {retrying ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <RefreshCw className="size-4" />
            )}
            Retry
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
