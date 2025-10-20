import { listConnectionsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { RepositoryResponse } from '@/api/client/types.gen'
import {
  Card,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { GitBranch, Search } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { listRepositoriesByConnection } from '@/api/client/sdk.gen'

interface RepositorySelectorProps {
  value: RepositoryResponse | null
  onChange: (repository: RepositoryResponse, connectionId: number) => void

  // Optional filtering
  owner?: string
  name?: string
  preferredConnectionId?: number

  // UI customization
  className?: string
  showSearch?: boolean
  autoSelectIfOneMatch?: boolean
}

export function RepositorySelector({
  value,
  onChange,
  owner,
  name,
  preferredConnectionId,
  className,
  showSearch = true,
  autoSelectIfOneMatch = false,
}: RepositorySelectorProps) {
  const [searchTerm, setSearchTerm] = useState('')
  const [repositories, setRepositories] = useState<
    Array<{ repo: RepositoryResponse; connectionId: number }>
  >([])
  const [isSearching, setIsSearching] = useState(false)

  // Fetch all git provider connections
  const { data: connections, isLoading: connectionsLoading } = useQuery({
    ...listConnectionsOptions(),
  })

  // Search repositories across all connections
  useEffect(() => {
    if (!connections?.connections) return

    const searchRepos = async () => {
      setIsSearching(true)
      const allRepos: Array<{
        repo: RepositoryResponse
        connectionId: number
      }> = []

      // Build prioritized list of connections
      const orderedConnections = [...connections.connections]
      if (preferredConnectionId) {
        orderedConnections.sort((a, b) =>
          a.id === preferredConnectionId
            ? -1
            : b.id === preferredConnectionId
              ? 1
              : 0
        )
      }

      for (const connection of orderedConnections) {
        try {
          const { data } = await listRepositoriesByConnection({
            path: { connection_id: connection.id },
            query: {
              search: name || searchTerm || '',
              per_page: 100,
            },
            throwOnError: true,
          })

          if (data?.repositories) {
            data.repositories.forEach((repo) => {
              allRepos.push({ repo, connectionId: connection.id })
            })
          }
        } catch (error) {
          console.error(
            `Error fetching repositories from connection ${connection.id}:`,
            error
          )
        }
      }

      setRepositories(allRepos)
      setIsSearching(false)

      // Auto-select if only one match and autoSelectIfOneMatch is true
      if (autoSelectIfOneMatch && allRepos.length === 1 && !value) {
        onChange(allRepos[0].repo, allRepos[0].connectionId)
      }
    }

    // Debounce search by 300ms
    const timeoutId = setTimeout(() => {
      searchRepos()
    }, 300)

    return () => clearTimeout(timeoutId)
  }, [
    connections,
    searchTerm,
    name,
    preferredConnectionId,
    autoSelectIfOneMatch,
    onChange,
    value,
  ])

  // Filter repositories based on owner/name if provided
  const filteredRepositories = useMemo(() => {
    if (!repositories.length) return []

    let filtered = repositories

    // If owner and name are specified, prioritize exact matches
    if (owner && name) {
      const exactMatch = filtered.find(
        (r) =>
          r.repo.owner === owner &&
          (r.repo.name === name || r.repo.full_name === `${owner}/${name}`)
      )

      if (exactMatch) {
        // Auto-select exact match if no value is set
        if (!value && autoSelectIfOneMatch) {
          onChange(exactMatch.repo, exactMatch.connectionId)
        }
        return [exactMatch]
      }

      // Filter by owner/name pattern
      filtered = filtered.filter(
        (r) =>
          r.repo.owner?.toLowerCase().includes(owner.toLowerCase()) &&
          r.repo.name?.toLowerCase().includes(name.toLowerCase())
      )
    }

    // Apply search term filter
    if (searchTerm && !name) {
      const term = searchTerm.toLowerCase()
      filtered = filtered.filter(
        (r) =>
          r.repo.name?.toLowerCase().includes(term) ||
          r.repo.full_name?.toLowerCase().includes(term) ||
          r.repo.owner?.toLowerCase().includes(term)
      )
    }

    return filtered
  }, [
    repositories,
    owner,
    name,
    searchTerm,
    value,
    autoSelectIfOneMatch,
    onChange,
  ])

  const isLoading = connectionsLoading || isSearching

  return (
    <div className={cn('space-y-3', className)}>
      {showSearch && (
        <div className="flex items-center gap-2">
          <Search className="h-4 w-4 text-muted-foreground" />
          <Input
            placeholder="Search repositories..."
            value={searchTerm}
            onChange={(e) => setSearchTerm(e.target.value)}
            className="flex-1"
            disabled={!!name} // Disable if searching for specific repo
            autoFocus
          />
        </div>
      )}

      {isLoading ? (
        <div className="space-y-2">
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      ) : filteredRepositories.length === 0 ? (
        <Card className="border-dashed">
          <CardHeader className="text-center py-8">
            <CardTitle className="text-base">No repositories found</CardTitle>
            <CardDescription>
              {name
                ? `Unable to find repository "${owner}/${name}"`
                : 'Try adjusting your search or add a Git provider connection'}
            </CardDescription>
          </CardHeader>
        </Card>
      ) : (
        <RadioGroup
          value={value?.id?.toString() || ''}
          onValueChange={(repoId) => {
            const selected = filteredRepositories.find(
              (r) => r.repo.id?.toString() === repoId
            )
            if (selected) {
              onChange(selected.repo, selected.connectionId)
            }
          }}
        >
          <div className="space-y-2">
            {filteredRepositories.map(({ repo, connectionId }) => (
              <Card
                key={`${connectionId}-${repo.id}`}
                className={cn(
                  'cursor-pointer transition-all hover:bg-muted/50',
                  value?.id === repo.id && 'ring-2 ring-primary'
                )}
                onClick={() => onChange(repo, connectionId)}
              >
                <CardHeader className="py-3">
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-3">
                      <GitBranch className="h-5 w-5 text-primary" />
                      <div>
                        <CardTitle className="text-sm">{repo.name}</CardTitle>
                        <CardDescription className="text-xs">
                          {repo.full_name}
                        </CardDescription>
                      </div>
                    </div>
                    <RadioGroupItem value={repo.id?.toString() || ''} />
                  </div>
                </CardHeader>
              </Card>
            ))}
          </div>
        </RadioGroup>
      )}
    </div>
  )
}
