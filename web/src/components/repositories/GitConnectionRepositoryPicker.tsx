// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { listGitProvidersOptions } from '@/api/client/@tanstack/react-query.gen'
import type {
  ConnectionResponse,
  RepositoryResponse,
} from '@/api/client/types.gen'
import { ProviderLogo } from '@/components/git/ProviderLogo'
import { RepositoryList } from '@/components/repositories/RepositoryList'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

/** The slice of a Git connection the picker needs. */
export type PickerConnection = Pick<
  ConnectionResponse,
  'id' | 'provider_id' | 'account_name' | 'account_type'
>

interface GitConnectionRepositoryPickerProps {
  connections: readonly PickerConnection[]
  /** The connection whose repositories are listed. */
  connectionId: number | null
  onConnectionChange: (connectionId: number) => void
  onRepositorySelect: (repository: RepositoryResponse) => void
  /** Highlights this repository in the list when set. */
  selectedRepositoryId?: number
  itemsPerPage?: number
}

/**
 * The repository picker of the new-service flow: a Git connection selector
 * above the searchable, filterable, sortable and paginated repository list
 * (with sync and framework badges). Shared by every page that points
 * something at a repository, so they browse repositories the same way.
 */
export function GitConnectionRepositoryPicker({
  connections,
  connectionId,
  onConnectionChange,
  onRepositorySelect,
  selectedRepositoryId,
  itemsPerPage = 15,
}: GitConnectionRepositoryPickerProps) {
  // Providers pick the right mark per connection (a GitLab connection must
  // not render the GitHub icon).
  const { data: providers } = useQuery({ ...listGitProvidersOptions() })
  const providerTypeOf = (providerId: number): string | undefined =>
    providers?.find((p) => p.id === providerId)?.provider_type

  const selected = connections.find((c) => c.id === connectionId)

  return (
    <div className="space-y-3">
      <Select
        value={connectionId === null ? undefined : String(connectionId)}
        onValueChange={(value) => onConnectionChange(Number(value))}
      >
        <SelectTrigger className="w-full" aria-label="Git connection">
          <SelectValue placeholder="Select Connection">
            {selected && (
              <ConnectionLabel
                connection={selected}
                providerType={providerTypeOf(selected.provider_id)}
              />
            )}
          </SelectValue>
        </SelectTrigger>
        <SelectContent>
          {connections.map((connection) => (
            <SelectItem key={connection.id} value={String(connection.id)}>
              <ConnectionLabel
                connection={connection}
                providerType={providerTypeOf(connection.provider_id)}
              />
            </SelectItem>
          ))}
        </SelectContent>
      </Select>

      {connectionId !== null && (
        <RepositoryList
          connectionId={connectionId}
          onRepositorySelect={onRepositorySelect}
          selectedRepositoryId={selectedRepositoryId}
          showSelection={selectedRepositoryId !== undefined}
          itemsPerPage={itemsPerPage}
          showHeader
          compactMode
          providerType={
            selected ? providerTypeOf(selected.provider_id) : undefined
          }
        />
      )}
    </div>
  )
}

function ConnectionLabel({
  connection,
  providerType,
}: {
  connection: PickerConnection
  providerType: string | undefined
}) {
  return (
    <div className="flex items-center gap-2">
      <ProviderLogo providerType={providerType} className="h-4 w-4" />
      <span className="font-medium">{connection.account_name}</span>
      <span className="text-xs text-muted-foreground">
        ({connection.account_type})
      </span>
    </div>
  )
}
