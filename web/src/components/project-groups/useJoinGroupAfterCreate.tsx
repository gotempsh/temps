// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useRef, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { useMutation, useQuery } from '@tanstack/react-query'
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
  /**
   * Set when a deployment is waiting on the join (see `joinBeforeDeploy`):
   * Retry joins and then runs it, so the first deployment still starts after
   * the service belongs to the project.
   */
  resume: (() => Promise<void>) | null
}

type JoinOutcome =
  | { status: 'joined'; group: ProjectGroupResponse }
  | { status: 'failed'; message: string }

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
 *
 * Ordering matters when the flow also deploys the service: the server
 * resolves the project's shared variables while it plans a deployment, so a
 * deployment started before the service joined runs without them. Flows that
 * start the deployment themselves use `joinBeforeDeploy` and `continueTo`.
 */
export function useJoinGroupAfterCreate() {
  const navigate = useNavigate()
  const invalidate = useInvalidateGroups()
  const { groupId, group } = useTargetGroup()
  const [failure, setFailure] = useState<JoinFailure | null>(null)
  // Where `continueTo` lands once the service has joined. Only ever read from
  // event handlers, never during render.
  const joinedGroupRef = useRef<ProjectGroupResponse | null>(null)

  /**
   * Add `service` to the target project and report how it went. Silent: no
   * toast, dialog or navigation, for a flow that owns its own failure UI.
   */
  const addToGroup = useCallback(
    async (service: CreatedService): Promise<JoinOutcome> => {
      if (groupId === null) {
        return { status: 'failed', message: 'No project was selected.' }
      }
      try {
        const { data } = await addProjectToGroup({
          path: { group_id: groupId, project_id: service.id },
          throwOnError: true,
        })
        joinedGroupRef.current = data
        await invalidate()
        return { status: 'joined', group: data }
      } catch (err) {
        await invalidate()
        return {
          status: 'failed',
          message: getErrorMessage(
            err,
            'The service could not be added to the project.'
          ),
        }
      }
    },
    [groupId, invalidate]
  )

  const reportFailure = useCallback(
    (
      service: CreatedService,
      message: string,
      resume: (() => Promise<void>) | null
    ) => {
      toast.error(
        `${service.name} was created, but not added to ${group?.name ?? 'the project'}`,
        { description: message }
      )
      setFailure({ service, message, resume })
    },
    [group]
  )

  /**
   * Finish a creation that deploys nothing itself: join the target project
   * when there is one, otherwise navigate to `fallbackHref`. Resolves false
   * when joining failed and the dialog is showing; the caller stays where
   * it is.
   */
  const finish = useCallback(
    async (service: CreatedService, fallbackHref: string): Promise<boolean> => {
      if (groupId === null) {
        navigate(fallbackHref)
        return true
      }
      const outcome = await addToGroup(service)
      if (outcome.status === 'failed') {
        reportFailure(service, outcome.message, null)
        return false
      }
      setFailure(null)
      toast.success(`${service.name} added to ${outcome.group.name}`)
      navigate(groupHref(outcome.group.slug))
      return true
    },
    [groupId, addToGroup, navigate, reportFailure]
  )

  /**
   * Add a just-created service to the target project BEFORE the caller
   * starts its first deployment, so that deployment is planned with the
   * project's shared variables. Resolves true when the caller should go on
   * to deploy (joined, or there is no target project).
   *
   * Resolves false when joining failed: nothing must be deployed. The dialog
   * is showing; its Retry joins again and, once that works, calls `resume`,
   * which must do the deployment and finish with `continueTo`. `resume` is
   * responsible for reporting its own failures.
   */
  const joinBeforeDeploy = useCallback(
    async (
      service: CreatedService,
      resume: () => Promise<void>
    ): Promise<boolean> => {
      if (groupId === null) return true
      const outcome = await addToGroup(service)
      if (outcome.status === 'failed') {
        reportFailure(service, outcome.message, resume)
        return false
      }
      setFailure(null)
      toast.success(`${service.name} added to ${outcome.group.name}`)
      return true
    },
    [groupId, addToGroup, reportFailure]
  )

  /**
   * Where a flow lands after it deployed: the project the service joined,
   * or `fallbackHref` when there was no target project.
   */
  const continueTo = useCallback(
    (fallbackHref: string) => {
      const joined = groupId === null ? null : joinedGroupRef.current
      navigate(joined ? groupHref(joined.slug) : fallbackHref)
    },
    [groupId, navigate]
  )

  const retryJoin = useMutation({
    mutationFn: async (pending: JoinFailure) => {
      const outcome = await addToGroup(pending.service)
      if (outcome.status === 'failed') {
        setFailure({ ...pending, message: outcome.message })
        return
      }
      setFailure(null)
      toast.success(`${pending.service.name} added to ${outcome.group.name}`)
      if (pending.resume) {
        await pending.resume()
      } else {
        navigate(groupHref(outcome.group.slug))
      }
    },
  })

  const joinGroupDialog = (
    <JoinGroupFailedDialog
      failure={failure}
      group={group}
      retrying={retryJoin.isPending}
      onRetry={() => failure && retryJoin.mutate(failure)}
      onClose={() => setFailure(null)}
    />
  )

  return {
    groupId,
    group,
    finish,
    joinBeforeDeploy,
    continueTo,
    addToGroup,
    joinGroupDialog,
  }
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
            {failure?.resume
              ? 'Nothing was deployed: the first deployment waits until the service is in the project, so it starts with the shared variables of the project. Retry to add it and deploy, or open the service and deploy it from there.'
              : 'The service exists and works on its own. Retry adding it, or add it later from the Add services menu of the project.'}
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
