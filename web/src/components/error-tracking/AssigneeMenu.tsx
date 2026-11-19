// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Users, UserRound } from 'lucide-react'
import {
  getErrorGroupQueryKey,
  listErrorGroupsQueryKey,
  searchErrorGroupAssigneesOptions,
  setErrorGroupAssigneeMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { GroupAssignee } from '@/api/client/types.gen'
import { Avatar, AvatarFallback } from '@/components/ui/avatar'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { queryKeyPrefix } from '@/lib/refresh'
import { assigneeInitials, assigneeLabel } from './assignee-label'

export interface AssigneeTarget {
  user_id?: number
  team_id?: number
}

interface AssigneeMenuProps {
  projectId: number
  groupId: number
  assignee: GroupAssignee | null | undefined
  /** The read-only free-text assignee of a group from before assignees were users. */
  legacyAssignedTo?: string | null
  currentUserId?: number
  /** `avatar` for list rows, `button` for the group page. */
  variant?: 'avatar' | 'button'
}

/**
 * Assign an error group to a user or a team (ADR 074, D1.6). Offers
 * "Assign to me", "Unassign" and a search over the people and teams who can
 * be assigned; the search runs only while the menu is open.
 */
export function AssigneeMenu({
  projectId,
  groupId,
  assignee,
  legacyAssignedTo,
  currentUserId,
  variant = 'button',
}: AssigneeMenuProps) {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState('')
  const label = assigneeLabel(assignee, legacyAssignedTo)

  const candidates = useQuery({
    ...searchErrorGroupAssigneesOptions({
      path: { project_id: projectId },
      query: { q: search.trim() },
    }),
    enabled: open,
    staleTime: 30_000,
  })

  const mutation = useMutation({
    ...setErrorGroupAssigneeMutation(),
    meta: { errorTitle: 'Failed to update the assignee' },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: queryKeyPrefix(listErrorGroupsQueryKey, {
          path: { project_id: projectId },
        }),
      })
      queryClient.invalidateQueries({
        queryKey: getErrorGroupQueryKey({
          path: { project_id: projectId, group_id: groupId },
        }),
      })
    },
  })
  const assign = (body: AssigneeTarget) => {
    setOpen(false)
    mutation.mutate({
      path: { project_id: projectId, group_id: groupId },
      body,
    })
  }

  const isMine =
    assignee?.type === 'user' &&
    currentUserId !== undefined &&
    assignee.id === currentUserId
  const users = candidates.data?.users ?? []
  const teams = candidates.data?.teams ?? []

  const icon = assignee ? (
    <Avatar className={variant === 'avatar' ? 'h-6 w-6' : 'h-5 w-5'}>
      <AvatarFallback className="text-[10px]">
        {assignee.type === 'team' ? (
          <Users className="h-3 w-3" />
        ) : (
          assigneeInitials(assignee)
        )}
      </AvatarFallback>
    </Avatar>
  ) : variant === 'avatar' ? (
    <span className="flex h-6 w-6 items-center justify-center rounded-full border border-dashed border-muted-foreground/40 text-muted-foreground">
      <UserRound className="h-3.5 w-3.5" />
    </span>
  ) : (
    <UserRound className="h-4 w-4 text-muted-foreground" />
  )

  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger asChild>
        {variant === 'avatar' ? (
          <button
            type="button"
            onClick={(e) => e.stopPropagation()}
            className="shrink-0 rounded-full outline-none transition-shadow hover:ring-2 hover:ring-border focus-visible:ring-2 focus-visible:ring-ring"
            aria-label={`Assignee: ${label}`}
            title={label}
            disabled={mutation.isPending}
          >
            {icon}
          </button>
        ) : (
          <Button
            variant="outline"
            size="sm"
            disabled={mutation.isPending}
            className="gap-2"
            aria-label={`Assignee: ${label}`}
          >
            {icon}
            {label}
          </Button>
        )}
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="end"
        className="w-64"
        onClick={(e) => e.stopPropagation()}
      >
        <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
          {label}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        {!isMine && currentUserId !== undefined && (
          <DropdownMenuItem onClick={() => assign({ user_id: currentUserId })}>
            Assign to me
          </DropdownMenuItem>
        )}
        {(assignee || legacyAssignedTo) && (
          <DropdownMenuItem onClick={() => assign({})}>
            Unassign
          </DropdownMenuItem>
        )}
        <DropdownMenuSeparator />
        <div className="px-2 py-1.5">
          <Input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => e.stopPropagation()}
            placeholder="Find a person or team"
            aria-label="Find a person or team to assign"
            className="h-8"
            maxLength={100}
          />
        </div>
        <div className="max-h-60 overflow-y-auto">
          {candidates.isLoading && (
            <p className="px-2 py-1.5 text-xs text-muted-foreground">
              Searching…
            </p>
          )}
          {candidates.isError && (
            <p className="px-2 py-1.5 text-xs text-destructive">
              Could not load people and teams.
            </p>
          )}
          {!candidates.isLoading &&
            !candidates.isError &&
            users.length === 0 &&
            teams.length === 0 && (
              <p className="px-2 py-1.5 text-xs text-muted-foreground">
                Nobody matches. Only people who can open this project are
                listed.
              </p>
            )}
          {users.map((user) => (
            <DropdownMenuItem
              key={`user-${user.id}`}
              onClick={() => assign({ user_id: user.id })}
            >
              <span className="flex min-w-0 flex-col">
                <span className="truncate">{user.name}</span>
                {user.email && (
                  <span className="truncate text-xs text-muted-foreground">
                    {user.email}
                  </span>
                )}
              </span>
            </DropdownMenuItem>
          ))}
          {teams.length > 0 && (
            <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
              Teams
            </DropdownMenuLabel>
          )}
          {teams.map((team) => (
            <DropdownMenuItem
              key={`team-${team.id}`}
              onClick={() => assign({ team_id: team.id })}
            >
              <Users className="mr-2 h-4 w-4 shrink-0" />
              <span className="truncate">{team.name}</span>
            </DropdownMenuItem>
          ))}
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
