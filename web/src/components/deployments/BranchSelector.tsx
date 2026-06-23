import {
  getRepositoryBranchesOptions,
  getPublicBranchesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
} from '@/components/ui/command'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Skeleton } from '@/components/ui/skeleton'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { cn } from '@/lib/utils'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  Check,
  ChevronsUpDown,
  CornerDownLeft,
  GitBranch,
  Key,
  Lock,
  RefreshCw,
} from 'lucide-react'
import { useMemo, useState, useEffect } from 'react'
import { isExpiredTokenError } from '@/utils/errorHandling'
import { Link } from 'react-router-dom'

/** Detect git provider from a git URL */
function detectProviderFromUrl(gitUrl: string): 'github' | 'gitlab' | null {
  if (gitUrl.includes('github.com') || gitUrl.includes('github'))
    return 'github'
  if (gitUrl.includes('gitlab.com') || gitUrl.includes('gitlab'))
    return 'gitlab'
  return null
}

/** Normalized branch shape the combobox renders, regardless of source. */
interface ResolvedBranch {
  name: string
  commit_sha?: string
  protected?: boolean
  /** This branch is the repo's default. */
  isDefault: boolean
  /** Synthetic entry for a selected ref the API didn't return. */
  isCurrentOutOfList?: boolean
}

interface BranchSelectorProps {
  repoOwner: string
  repoName: string
  connectionId?: number // Optional for public repos
  defaultBranch?: string
  value?: string
  onChange: (branch: string) => void
  onError?: (error: string | null) => void
  onBranchesLoaded?: (branches: string[]) => void
  disabled?: boolean
  /** Pre-loaded branches (for public repos or when already fetched) */
  branches?: Array<{ name: string; is_default?: boolean }>
  /** Git URL for public repos without a provider connection */
  gitUrl?: string | null
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
  branches: providedBranches,
  gitUrl,
}: BranchSelectorProps) {
  const queryClient = useQueryClient()

  // Detect if this is a public repo (no connection but has gitUrl)
  const publicProvider = useMemo(() => {
    if (connectionId) return null
    if (gitUrl) return detectProviderFromUrl(gitUrl)
    // Default to github if we have owner/name but no connection
    if (repoOwner && repoName) return 'github' as const
    return null
  }, [connectionId, gitUrl, repoOwner, repoName])

  // Fetch branches from authenticated API (when connectionId exists)
  const branchesQuery = useQuery({
    ...getRepositoryBranchesOptions({
      path: {
        owner: repoOwner,
        repo: repoName,
      },
      query: {
        connection_id: connectionId!,
        fresh: false,
      },
    }),
    enabled: !providedBranches && !!repoOwner && !!repoName && !!connectionId,
    retry: false,
  })

  // Fetch branches from public API (when no connectionId but public repo)
  const publicBranchesQuery = useQuery({
    ...getPublicBranchesOptions({
      path: {
        provider: publicProvider || 'github',
        owner: repoOwner,
        repo: repoName,
      },
    }),
    enabled:
      !providedBranches &&
      !!repoOwner &&
      !!repoName &&
      !connectionId &&
      !!publicProvider,
    retry: false,
  })

  // Merge the two queries
  const effectiveQuery = connectionId ? branchesQuery : publicBranchesQuery

  const handleRefresh = async () => {
    if (connectionId) {
      // Refresh authenticated branches
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
    } else if (publicProvider) {
      // Refresh public branches
      await queryClient.invalidateQueries({
        queryKey: getPublicBranchesOptions({
          path: {
            provider: publicProvider,
            owner: repoOwner,
            repo: repoName,
          },
        }).queryKey,
      })
    }
  }

  // Check if branches query has expired token error
  const hasExpiredToken = useMemo(
    () => branchesQuery.error && isExpiredTokenError(branchesQuery.error),
    [branchesQuery.error]
  )

  // The repo's default branch — prefer the explicit prop, fall back to an
  // `is_default` flag on pre-loaded branches.
  const effectiveDefaultName = useMemo(() => {
    if (defaultBranch) return defaultBranch
    return providedBranches?.find((b) => b.is_default)?.name
  }, [defaultBranch, providedBranches])

  // Sort branches: default first, then common main branches, then alphabetical.
  const sortedBranches = useMemo<ResolvedBranch[]>(() => {
    const raw = providedBranches ?? effectiveQuery.data?.branches
    if (!raw) return []

    const mainBranches = ['main', 'master', 'develop']
    return raw
      .map<ResolvedBranch>((b) => ({
        name: b.name,
        commit_sha: 'commit_sha' in b ? b.commit_sha : undefined,
        protected: 'protected' in b ? b.protected : undefined,
        isDefault: b.name === effectiveDefaultName,
      }))
      .sort((a, b) => {
        if (a.isDefault) return -1
        if (b.isDefault) return 1
        const aIsMain = mainBranches.includes(a.name)
        const bIsMain = mainBranches.includes(b.name)
        if (aIsMain && !bIsMain) return -1
        if (!aIsMain && bIsMain) return 1
        return a.name.localeCompare(b.name)
      })
  }, [providedBranches, effectiveQuery.data, effectiveDefaultName])

  const effectiveBranch = value ?? ''

  // Always include the currently-selected branch, even when the API didn't
  // return it (huge branch counts hitting the safety cap, a branch renamed or
  // deleted upstream, or an unauthenticated fetch that excluded it). Without
  // this the trigger would show the placeholder for a branch that is actually
  // set.
  const displayBranches = useMemo<ResolvedBranch[]>(() => {
    if (!effectiveBranch) return sortedBranches
    if (sortedBranches.some((b) => b.name === effectiveBranch)) {
      return sortedBranches
    }
    return [
      {
        name: effectiveBranch,
        isDefault: effectiveBranch === effectiveDefaultName,
        isCurrentOutOfList: true,
      },
      ...sortedBranches,
    ]
  }, [sortedBranches, effectiveBranch, effectiveDefaultName])

  // Notify parent when branches are loaded
  useEffect(() => {
    if (sortedBranches.length > 0 && onBranchesLoaded) {
      onBranchesLoaded(sortedBranches.map((b) => b.name))
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sortedBranches])

  if (hasExpiredToken) {
    return (
      <Alert variant="destructive">
        <AlertTriangle className="h-4 w-4" />
        <AlertTitle>Authentication Required</AlertTitle>
        <AlertDescription>
          <p className="mb-2">
            Your Git provider token has expired. Please reconnect to continue.
          </p>
          <Link to="/git-providers">
            <Button type="button" variant="outline" size="sm">
              <Key className="mr-2 h-4 w-4" />
              Manage Git Providers
            </Button>
          </Link>
        </AlertDescription>
      </Alert>
    )
  }

  if (effectiveQuery.isLoading) {
    return (
      <div className="flex gap-2">
        <Skeleton className="h-10 flex-1" />
        <Skeleton className="h-10 w-10 shrink-0" />
      </div>
    )
  }

  if (effectiveQuery.error) {
    return (
      <Alert variant="destructive">
        <AlertTriangle className="h-4 w-4" />
        <AlertTitle>Error Loading Branches</AlertTitle>
        <AlertDescription>
          {effectiveQuery.error instanceof Error
            ? effectiveQuery.error.message
            : 'Failed to load branches from repository'}
        </AlertDescription>
      </Alert>
    )
  }

  // When we have no way to enumerate branches (no connection, no public
  // provider, no pre-loaded list), fall back to free-text entry.
  const canEnumerate =
    !!providedBranches ||
    (!!repoOwner && !!repoName && (!!connectionId || !!publicProvider))

  if (!canEnumerate) {
    return (
      <Input
        value={effectiveBranch}
        onChange={(e) => {
          onChange(e.target.value)
          onError?.(null)
        }}
        placeholder={`Enter branch name${effectiveDefaultName ? ` (default: ${effectiveDefaultName})` : ''}`}
        disabled={disabled}
      />
    )
  }

  return (
    <div className="flex gap-2">
      <BranchCombobox
        branches={displayBranches}
        value={effectiveBranch}
        defaultName={effectiveDefaultName}
        disabled={disabled}
        onSelect={(branch) => {
          onChange(branch)
          onError?.(null)
        }}
      />
      <Button
        type="button"
        variant="outline"
        size="icon"
        className="shrink-0"
        onClick={handleRefresh}
        disabled={effectiveQuery.isFetching || disabled}
        title="Refresh branches"
      >
        <RefreshCw
          className={cn('h-4 w-4', effectiveQuery.isFetching && 'animate-spin')}
        />
      </Button>
    </div>
  )
}

/** Cap rendered rows so very large branch lists stay snappy. */
const MAX_VISIBLE = 200

// Identity sentinel for the "use this custom ref" row. The trailing space makes
// it impossible to collide with a real git ref (refs can't contain spaces) and
// keeps it a plain, DOM-attribute-safe string for cmdk's data-value matching.
const CUSTOM_PREFIX = 'use-custom-ref '

interface BranchComboboxProps {
  branches: ResolvedBranch[]
  value: string
  defaultName?: string
  disabled?: boolean
  onSelect: (branch: string) => void
}

function BranchCombobox({
  branches,
  value,
  defaultName,
  disabled,
  onSelect,
}: BranchComboboxProps) {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  // The currently-highlighted row, driven manually (see firstItemValue below).
  const [activeValue, setActiveValue] = useState('')

  const trimmedQuery = query.trim()

  // Rank + filter against the query. Prefix and word-boundary matches rank
  // above mid-string and subsequence matches; ties keep the incoming sort
  // (default → main → alphabetical).
  const filtered = useMemo(() => {
    if (!trimmedQuery) return branches
    return branches
      .map((b, i) => ({ b, score: scoreBranch(b.name, trimmedQuery), i }))
      .filter((x) => x.score > 0)
      .sort((a, b) => b.score - a.score || a.i - b.i)
      .map((x) => x.b)
  }, [branches, trimmedQuery])

  const visible = filtered.slice(0, MAX_VISIBLE)
  const hiddenCount = filtered.length - visible.length

  // Offer a "use this exact ref" row when the query doesn't match any known
  // branch name — lets users target an arbitrary branch, tag, or one that
  // hasn't been fetched.
  const showCustom =
    !!trimmedQuery && !branches.some((b) => b.name === trimmedQuery)
  const customValue = `${CUSTOM_PREFIX}${trimmedQuery}`

  // Split into a pinned block (current + default) and the rest, but only when
  // not actively searching — search results read better as one flat ranked list.
  const isSearching = !!trimmedQuery
  const pinned = isSearching
    ? []
    : visible.filter((b) => b.isCurrentOutOfList || b.isDefault)
  const rest = isSearching
    ? visible
    : visible.filter((b) => !b.isCurrentOutOfList && !b.isDefault)

  // cmdk owns the active-item pointer, but with shouldFilter={false} it stops
  // re-pointing it when we re-rank the list. Drive the highlight ourselves so
  // Enter always targets the visually top row (and the custom row when nothing
  // matches). `onValueChange` keeps arrow-key/pointer moves working; this effect
  // snaps the highlight back to the top whenever the result head changes.
  const firstItemValue =
    (pinned[0] ?? rest[0])?.name ?? (showCustom ? customValue : '')
  useEffect(() => {
    setActiveValue(firstItemValue)
  }, [firstItemValue])

  const handleSelect = (branch: string) => {
    onSelect(branch)
    setOpen(false)
    setQuery('')
  }

  const selectedIsDefault = !!value && value === defaultName

  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next)
        if (!next) setQuery('')
      }}
    >
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          role="combobox"
          aria-expanded={open}
          disabled={disabled}
          className={cn(
            'h-10 flex-1 min-w-0 justify-between font-normal',
            !value && 'text-muted-foreground'
          )}
        >
          <span className="flex min-w-0 items-center gap-2">
            <GitBranch className="h-4 w-4 shrink-0 opacity-60" />
            <span className={cn('truncate', value && 'font-mono text-[13px]')}>
              {value || 'Select a branch'}
            </span>
            {selectedIsDefault && (
              <Badge
                variant="secondary"
                className="shrink-0 rounded px-1.5 py-0 text-[10px] font-normal"
              >
                default
              </Badge>
            )}
          </span>
          <ChevronsUpDown className="ml-2 h-4 w-4 shrink-0 opacity-50" />
        </Button>
      </PopoverTrigger>
      {/* 420px (vs the generic 360px combobox) because rows are denser — name
          + SHA + lock + badges; max-h caps to the viewport on short screens. */}
      <PopoverContent
        className="w-[min(calc(100vw-2rem),420px)] min-w-[var(--radix-popover-trigger-width)] p-0"
        align="start"
      >
        <Command
          shouldFilter={false}
          value={activeValue}
          onValueChange={setActiveValue}
        >
          <CommandInput
            placeholder="Search or enter a branch..."
            value={query}
            onValueChange={setQuery}
          />
          <CommandList className="max-h-[min(60vh,360px)]">
            {visible.length === 0 && !showCustom && (
              <CommandEmpty>No branches found.</CommandEmpty>
            )}

            {pinned.length > 0 && (
              <CommandGroup>
                {pinned.map((branch) => (
                  <BranchRow
                    key={branch.name}
                    branch={branch}
                    selected={value === branch.name}
                    onSelect={handleSelect}
                  />
                ))}
              </CommandGroup>
            )}

            {pinned.length > 0 && rest.length > 0 && <CommandSeparator />}

            {rest.length > 0 && (
              <CommandGroup
                heading={
                  isSearching ? undefined : `Branches (${branches.length})`
                }
              >
                {rest.map((branch) => (
                  <BranchRow
                    key={branch.name}
                    branch={branch}
                    selected={value === branch.name}
                    onSelect={handleSelect}
                  />
                ))}
              </CommandGroup>
            )}

            {hiddenCount > 0 && (
              <div className="border-t px-2 py-1.5 text-center text-xs text-muted-foreground">
                {hiddenCount} more — keep typing to narrow results
              </div>
            )}

            {showCustom && (
              <CommandGroup heading="Custom">
                <CommandItem
                  value={customValue}
                  onSelect={() => handleSelect(trimmedQuery)}
                >
                  <CornerDownLeft className="mr-2 h-4 w-4 shrink-0 opacity-60" />
                  <span className="truncate">
                    Use{' '}
                    <span className="font-medium text-foreground">
                      &ldquo;{trimmedQuery}&rdquo;
                    </span>
                  </span>
                </CommandItem>
              </CommandGroup>
            )}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}

interface BranchRowProps {
  branch: ResolvedBranch
  selected: boolean
  onSelect: (branch: string) => void
}

function BranchRow({ branch, selected, onSelect }: BranchRowProps) {
  return (
    <CommandItem value={branch.name} onSelect={() => onSelect(branch.name)}>
      <Check
        aria-hidden
        className={cn(
          'mr-2 h-4 w-4 shrink-0',
          selected ? 'opacity-100' : 'opacity-0'
        )}
      />
      <GitBranch className="mr-2 h-4 w-4 shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1 truncate font-mono text-[13px]">
        {branch.name}
      </span>
      {selected && <span className="sr-only">(selected)</span>}
      <span className="flex shrink-0 items-center gap-1.5 pl-2">
        {branch.commit_sha && (
          <span
            className="font-mono text-[10px] text-muted-foreground"
            aria-label={`commit ${branch.commit_sha.slice(0, 7)}`}
          >
            {branch.commit_sha.slice(0, 7)}
          </span>
        )}
        {branch.protected && (
          <Lock
            className="!h-3 !w-3 text-muted-foreground"
            aria-label="Protected branch"
          />
        )}
        {branch.isCurrentOutOfList && (
          <Badge
            variant="outline"
            className="rounded px-1.5 py-0 text-[10px] font-normal"
          >
            current
          </Badge>
        )}
        {branch.isDefault && (
          <Badge
            variant="secondary"
            className="rounded px-1.5 py-0 text-[10px] font-normal"
          >
            default
          </Badge>
        )}
      </span>
    </CommandItem>
  )
}

/**
 * Score a branch name against a query. Higher is a better match; 0 means no
 * match (excluded). Prefix > word-boundary > substring > subsequence.
 */
function scoreBranch(name: string, query: string): number {
  const n = name.toLowerCase()
  const q = query.toLowerCase()
  const idx = n.indexOf(q)
  if (idx === 0) return 100
  if (idx > 0) {
    const prev = n[idx - 1]
    if (prev === '/' || prev === '-' || prev === '_') return 80 - idx * 0.01
    return 60 - idx * 0.01
  }
  return isSubsequence(n, q) ? 20 : 0
}

/** Does `needle` appear in `haystack` as an in-order subsequence? */
function isSubsequence(haystack: string, needle: string): boolean {
  if (!needle) return true
  let i = 0
  for (let j = 0; j < haystack.length && i < needle.length; j++) {
    if (haystack[j] === needle[i]) i++
  }
  return i === needle.length
}
