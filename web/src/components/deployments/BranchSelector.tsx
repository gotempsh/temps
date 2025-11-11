import { getRepositoryBranchesOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, Key, RefreshCw } from 'lucide-react'
import { useMemo, useState, useEffect } from 'react'
import { isExpiredTokenError } from '@/utils/errorHandling'
import { Link } from 'react-router-dom'

interface BranchSelectorProps {
  repoOwner: string
  repoName: string
  connectionId: number
  defaultBranch?: string
  value?: string
  onChange: (branch: string) => void
  onError?: (error: string | null) => void
  onBranchesLoaded?: (branches: string[]) => void
  disabled?: boolean
}

export function BranchSelector({
  repoOwner,
  repoName,
  connectionId,
  defaultBranch,
  value = '',
  onChange,
  onError,
  onBranchesLoaded,
  disabled = false,
}: BranchSelectorProps) {
  const [isCustomBranch, setIsCustomBranch] = useState(false)
  const queryClient = useQueryClient()

  // Fetch branches from repository (always with fresh=false for caching)
  const branchesQuery = useQuery({
    ...getRepositoryBranchesOptions({
      path: {
        owner: repoOwner,
        repo: repoName,
      },
      query: {
        connection_id: connectionId,
        fresh: false,
      },
    }),
    enabled: !!repoOwner && !!repoName && !!connectionId,
    retry: false,
  })

  const handleRefresh = async () => {
    // Fetch fresh data and update the cache
    const freshData = await queryClient.fetchQuery({
      ...getRepositoryBranchesOptions({
        path: {
          owner: repoOwner,
          repo: repoName,
        },
        query: {
          connection_id: connectionId,
          fresh: true,
        },
      }),
    })

    // Update the cached query with fresh data
    queryClient.setQueryData(
      getRepositoryBranchesOptions({
        path: {
          owner: repoOwner,
          repo: repoName,
        },
        query: {
          connection_id: connectionId,
          fresh: false,
        },
      }).queryKey,
      freshData
    )
  }

  // Check if branches query has expired token error
  const hasExpiredToken = useMemo(
    () => branchesQuery.error && isExpiredTokenError(branchesQuery.error),
    [branchesQuery.error]
  )

  // Sort branches: default branch first, then alphabetically
  const sortedBranches = useMemo(() => {
    if (!branchesQuery.data?.branches) return []

    return [...branchesQuery.data.branches].sort((a, b) => {
      // Default branch always comes first
      if (a.name === defaultBranch) return -1
      if (b.name === defaultBranch) return 1

      // Common main branches come next (main, master, develop)
      const mainBranches = ['main', 'master', 'develop']
      const aIsMain = mainBranches.includes(a.name)
      const bIsMain = mainBranches.includes(b.name)

      if (aIsMain && !bIsMain) return -1
      if (!aIsMain && bIsMain) return 1

      // Then alphabetically
      return a.name.localeCompare(b.name)
    })
  }, [branchesQuery.data, defaultBranch])

  const effectiveBranch = value || defaultBranch || ''

  // Notify parent when branches are loaded
  useEffect(() => {
    if (sortedBranches.length > 0 && onBranchesLoaded) {
      onBranchesLoaded(sortedBranches.map((b) => b.name))
    }
  }, [sortedBranches, onBranchesLoaded])

  if (hasExpiredToken) {
    return (
      <Alert variant="destructive">
        <AlertTriangle className="h-4 w-4" />
        <AlertTitle>Authentication Required</AlertTitle>
        <AlertDescription>
          <p className="mb-2">
            Your Git provider token has expired. Please reconnect to continue.
          </p>
          <Link to="/settings/git-providers">
            <Button variant="outline" size="sm">
              <Key className="mr-2 h-4 w-4" />
              Manage Git Providers
            </Button>
          </Link>
        </AlertDescription>
      </Alert>
    )
  }

  if (branchesQuery.isLoading) {
    return <Skeleton className="h-10 w-full" />
  }

  if (branchesQuery.error) {
    return (
      <Alert variant="destructive">
        <AlertTriangle className="h-4 w-4" />
        <AlertTitle>Error Loading Branches</AlertTitle>
        <AlertDescription>
          {branchesQuery.error instanceof Error
            ? branchesQuery.error.message
            : 'Failed to load branches from repository'}
        </AlertDescription>
      </Alert>
    )
  }

  if (sortedBranches.length > 0 && !isCustomBranch) {
    return (
      <div className="flex gap-2">
        <Select
          value={effectiveBranch}
          onValueChange={(val) => {
            if (val === '__custom__') {
              setIsCustomBranch(true)
              onChange('')
            } else {
              onChange(val)
              onError?.(null)
            }
          }}
          disabled={disabled}
        >
          <SelectTrigger>
            <SelectValue placeholder="Select a branch" />
          </SelectTrigger>
          <SelectContent>
            {sortedBranches.map((branch) => (
              <SelectItem key={branch.name} value={branch.name}>
                {branch.name}
                {branch.name === defaultBranch && ' (default)'}
              </SelectItem>
            ))}
            <SelectItem value="__custom__">Enter custom branch...</SelectItem>
          </SelectContent>
        </Select>
        <Button
          type="button"
          variant="outline"
          size="icon"
          onClick={handleRefresh}
          disabled={branchesQuery.isFetching || disabled}
        >
          <RefreshCw
            className={`h-4 w-4 ${branchesQuery.isFetching ? 'animate-spin' : ''}`}
          />
        </Button>
      </div>
    )
  }

  return (
    <>
      <Input
        value={effectiveBranch}
        onChange={(e) => {
          onChange(e.target.value)
          onError?.(null)
        }}
        placeholder={`Enter branch name${defaultBranch ? ` (default: ${defaultBranch})` : ''}`}
        disabled={disabled}
      />
      {isCustomBranch && sortedBranches.length > 0 && (
        <Button
          type="button"
          variant="link"
          size="sm"
          onClick={() => {
            setIsCustomBranch(false)
            onChange(defaultBranch || '')
          }}
          className="px-0"
        >
          Back to branch list
        </Button>
      )}
    </>
  )
}
