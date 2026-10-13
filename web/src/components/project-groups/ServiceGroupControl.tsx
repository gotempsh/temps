// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Boxes, Plus, RefreshCw } from 'lucide-react'
import {
  addProjectToGroupMutation,
  getProjectGroupMembershipOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { getErrorMessage } from '@/utils/errorHandling'
import { groupHref, serviceCount } from '@/lib/terminology'
import { badgeVariants } from '@/components/ui/badge'
import { cn } from '@/lib/utils'
import { Link } from 'react-router'
import { CreateProjectGroupDialog } from './CreateProjectGroupDialog'
import { useInvalidateGroups, useProjectGroups } from './useProjectGroups'

/** The project (group) a service belongs to, as a header chip. */
export function useServiceGroup(projectId: number | undefined) {
  return useQuery({
    ...getProjectGroupMembershipOptions({
      path: { project_id: projectId ?? 0 },
    }),
    enabled: Boolean(projectId),
    retry: false,
  })
}

/**
 * Shows which project a service belongs to, or offers to put it in one.
 * Standalone services get the entry point to grouping right where they are.
 */
export function ServiceGroupControl({
  project,
  inline = false,
}: {
  project: ProjectResponse
  /** Render the trigger at every width (default: header chip, md and up). */
  inline?: boolean
}) {
  const membership = useServiceGroup(project.id)
  const [open, setOpen] = useState(false)
  const [creating, setCreating] = useState(false)
  const { groups } = useProjectGroups()
  const invalidate = useInvalidateGroups()
  const add = useMutation({ ...addProjectToGroupMutation() })

  if (membership.isLoading) return null
  if (membership.isError) {
    // Quiet, but not silent: the chip is where grouping starts from.
    return (
      <Button
        variant="ghost"
        size="sm"
        className="h-6 shrink-0 gap-1 px-2 text-xs text-muted-foreground"
        title={getErrorMessage(
          membership.error,
          "This service's project could not be loaded."
        )}
        disabled={membership.isFetching}
        onClick={() => void membership.refetch()}
      >
        <RefreshCw
          className={membership.isFetching ? 'size-3 animate-spin' : 'size-3'}
          aria-hidden="true"
        />
        <span className="hidden sm:inline">Project unavailable ·</span> Retry
      </Button>
    )
  }

  const group = membership.data?.group
  if (group) {
    // Switching between its services happens on the service's name; this
    // badge says which project that is and opens it.
    return (
      <Link
        to={groupHref(group.slug)}
        title={`Part of project ${group.name} (${serviceCount(group.projects.length)}). Open its overview.`}
        className={cn(
          badgeVariants({ variant: 'secondary' }),
          'inline-flex shrink-0 gap-1 font-medium'
        )}
        aria-label={`Project ${group.name}`}
      >
        <Boxes className="size-3" aria-hidden="true" />
        <span className="hidden max-w-[200px] truncate sm:inline">
          {group.name}
        </span>
      </Link>
    )
  }

  const addTo = async (groupId: number, groupName: string) => {
    try {
      await add.mutateAsync({
        path: { group_id: groupId, project_id: project.id },
      })
      await invalidate()
      toast.success(`${project.name} is now part of ${groupName}`)
      setOpen(false)
    } catch (err) {
      toast.error(
        getErrorMessage(err, `Could not add ${project.name} to ${groupName}`)
      )
    }
  }

  return (
    <>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button
            variant="ghost"
            size="sm"
            className={
              inline
                ? 'h-7 shrink-0 gap-1 px-2 text-xs'
                : 'inline-flex h-6 shrink-0 gap-1 px-2 text-xs text-muted-foreground'
            }
            aria-label="Add to project"
          >
            <Plus className="size-3" aria-hidden="true" />
            <span className={inline ? undefined : 'hidden sm:inline'}>
              Add to project
            </span>
          </Button>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-72 p-2">
          <p className="px-2 pb-2 text-xs text-muted-foreground">
            Group {project.name} with the services it ships with to share
            variables and get one overview.
          </p>
          {groups.length > 0 && (
            <div className="max-h-56 overflow-y-auto border-y py-1">
              {groups.map((g) => (
                <button
                  key={g.id}
                  type="button"
                  disabled={add.isPending}
                  onClick={() => void addTo(g.id, g.name)}
                  className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm hover:bg-muted disabled:opacity-50"
                >
                  <Boxes
                    className="size-4 text-muted-foreground"
                    aria-hidden="true"
                  />
                  <span className="min-w-0 flex-1 truncate">{g.name}</span>
                  <span className="text-xs text-muted-foreground">
                    {serviceCount(g.projects.length)}
                  </span>
                </button>
              ))}
            </div>
          )}
          <button
            type="button"
            onClick={() => {
              setOpen(false)
              setCreating(true)
            }}
            className="mt-1 flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm hover:bg-muted"
          >
            <Plus className="size-4 text-muted-foreground" aria-hidden="true" />
            New project with {project.name}…
          </button>
        </PopoverContent>
      </Popover>
      <CreateProjectGroupDialog
        open={creating}
        onOpenChange={setCreating}
        initialServices={[{ id: project.id, name: project.name }]}
      />
    </>
  )
}
