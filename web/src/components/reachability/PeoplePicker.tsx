// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import {
  listTeamsOptions,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Checkbox } from '@/components/ui/checkbox'
import { Skeleton } from '@/components/ui/skeleton'
import { idList, toggleId } from '@/lib/reachability'

interface Choice {
  id: number
  label: string
  detail?: string
}

/**
 * Pick the teams or users who may sign in to a signed-in address (ADR 065
 * D7). The value is a comma-separated id list, as the form keeps it.
 */
export function PeoplePicker({
  kind,
  value,
  onChange,
}: {
  kind: 'teams' | 'users'
  value: string
  onChange: (next: string) => void
}) {
  const teams = useQuery({
    ...listTeamsOptions({ query: { page: 1, page_size: 100 } }),
    enabled: kind === 'teams',
  })
  const users = useQuery({
    ...listUsersOptions({ query: { include_deleted: false } }),
    enabled: kind === 'users',
  })
  const query = kind === 'teams' ? teams : users
  const choices: Choice[] =
    kind === 'teams'
      ? (teams.data?.teams ?? []).map((team) => ({
          id: team.id,
          label: team.name,
          detail: team.slug,
        }))
      : (users.data ?? []).map(({ user }) => ({
          id: user.id,
          label: user.name || user.email,
          detail: user.name ? user.email : undefined,
        }))
  const selected = new Set(idList(value))

  if (query.isPending)
    return (
      <div className="space-y-2">
        <Skeleton className="h-5 w-48" />
        <Skeleton className="h-5 w-40" />
      </div>
    )
  if (query.isError)
    return (
      <p className="text-sm text-muted-foreground">
        {kind === 'teams'
          ? 'Temps could not list the teams.'
          : 'Only instance admins can list people.'}{' '}
        Leave it empty to let everyone with access to the project sign in.
      </p>
    )
  if (choices.length === 0)
    return (
      <p className="text-sm text-muted-foreground">
        {kind === 'teams'
          ? 'There are no teams yet.'
          : 'There is nobody to list yet.'}
      </p>
    )
  return (
    <div className="max-h-40 space-y-1.5 overflow-y-auto rounded-md border p-2">
      {choices.map((choice) => (
        <label
          key={choice.id}
          className="flex cursor-pointer items-center gap-2 text-sm"
        >
          <Checkbox
            checked={selected.has(choice.id)}
            onCheckedChange={() => onChange(toggleId(value, choice.id))}
          />
          <span>{choice.label}</span>
          {choice.detail && (
            <span className="text-xs text-muted-foreground">
              {choice.detail}
            </span>
          )}
        </label>
      ))}
    </div>
  )
}
