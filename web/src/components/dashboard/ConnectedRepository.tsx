// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect, useMemo, useState } from 'react'
import { formatDistanceToNow, isValid } from 'date-fns'
import { Link, useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight, Search, Settings2, SlidersHorizontal } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  listPresetsOptions,
  listConnectionsOptions,
  listGitProvidersOptions,
  listRepositoriesByConnectionOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useDebounce } from '@/hooks/useDebounce'
import GithubIcon from '@/icons/Github'
import GitlabIcon from '@/icons/Gitlab'

const pageSize = 5

function providerTab(type: string | undefined) {
  const normalized = type?.toLowerCase()
  return normalized === 'github_app' ? 'github' : normalized
}

/** Real connected accounts and server-side repository search, shared by all builds. */
export function ConnectedRepository() {
  const [params, setParams] = useSearchParams()
  const connections = useQuery({
    ...listConnectionsOptions({ query: { per_page: 100 } }),
    refetchInterval: (query) =>
      query.state.data?.connections.some((c) => c.syncing) ? 2000 : false,
  })
  const providers = useQuery(listGitProvidersOptions())
  const available = connections.data?.connections ?? []
  const providerFor = (id: number) =>
    providerTab(providers.data?.find((p) => p.id === id)?.provider_type)
  const provider =
    providerTab(params.get('gitProvider') ?? undefined) ??
    providerFor(available[0]?.provider_id) ??
    'github'
  const accounts = available.filter(
    (c) => providerFor(c.provider_id) === provider
  )
  const account =
    accounts.find((c) => String(c.id) === params.get('gitConnection')) ??
    accounts[0]
  const preset = params.get('repoPreset') ?? ''
  const updatedDays = ['7', '30', '90'].includes(
    params.get('repoUpdated') ?? ''
  )
    ? params.get('repoUpdated')!
    : ''
  const [filterReferenceTime] = useState(() => Date.now())
  const updatedAfter = useMemo(
    () =>
      updatedDays
        ? new Date(
            filterReferenceTime - Number(updatedDays) * 86400000
          ).toISOString()
        : undefined,
    [updatedDays, filterReferenceTime]
  )
  const [filtersOpen, setFiltersOpen] = useState(Boolean(preset || updatedDays))
  const filterCount = Number(Boolean(preset)) + Number(Boolean(updatedDays))
  const presetCatalog = useQuery({
    ...listPresetsOptions(),
    enabled: filtersOpen,
  })
  const search = params.get('repoSearch') ?? ''
  const debouncedSearch = useDebounce(search, 250)
  const requestedPage = Number(params.get('repoPage') ?? '1')
  const page =
    Number.isSafeInteger(requestedPage) && requestedPage > 0 ? requestedPage : 1
  const ready =
    !!account &&
    account.is_active &&
    !account.is_expired &&
    account.has_authenticated_credentials
  const repos = useQuery({
    ...listRepositoriesByConnectionOptions({
      path: { connection_id: account?.id ?? 0 },
      query: {
        page,
        per_page: pageSize,
        search: debouncedSearch,
        sort: 'updated_at',
        direction: 'desc',
        preset: preset || undefined,
        updated_after: updatedAfter,
      },
    }),
    enabled: ready,
    refetchInterval: account?.syncing ? 2000 : false,
  })
  const total = repos.data?.total_count ?? 0
  const pages = Math.max(1, Math.ceil(total / pageSize))
  const loading = connections.isPending || providers.isPending
  const error = connections.isError || providers.isError
  function update(values: Record<string, string | null>, replace = false) {
    setParams(
      (previous) => {
        const next = new URLSearchParams(previous)
        for (const [key, value] of Object.entries(values)) {
          if (value) next.set(key, value)
          else next.delete(key)
        }
        return next
      },
      { replace }
    )
  }
  useEffect(() => {
    if (repos.isSuccess && !repos.isFetching && page > pages) {
      setParams(
        (previous) => {
          const next = new URLSearchParams(previous)
          next.set('repoPage', String(pages))
          return next
        },
        { replace: true }
      )
    }
  }, [repos.isSuccess, repos.isFetching, page, pages, setParams])

  return (
    <section
      aria-label="Deploy from a repository"
      className="min-w-0 space-y-4 rounded-lg border p-5"
    >
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <h3 className="text-sm font-semibold">From a repository</h3>
          <p className="mt-1 text-sm text-muted-foreground">
            Choose a repository to review its deployment settings.
          </p>
        </div>
      </div>
      <Tabs
        value={provider}
        onValueChange={(value) =>
          update({
            gitProvider: value,
            gitConnection: null,
            repoSearch: null,
            repoPage: null,
          })
        }
      >
        <TabsList aria-label="Git provider">
          <TabsTrigger value="github">
            <span aria-hidden="true">
              <GithubIcon className="size-4" />
            </span>
            GitHub
          </TabsTrigger>
          <TabsTrigger value="gitlab">
            <span aria-hidden="true">
              <GitlabIcon className="size-4" />
            </span>
            GitLab
          </TabsTrigger>
        </TabsList>
      </Tabs>
      {loading ? (
        <div role="status" aria-label="Loading Git accounts">
          <Skeleton className="h-10 w-full" />
        </div>
      ) : error ? (
        <div role="alert" className="space-y-2 text-sm">
          <p>Could not load Git accounts.</p>
          <Button
            variant="outline"
            onClick={() => {
              void connections.refetch()
              void providers.refetch()
            }}
          >
            Retry
          </Button>
        </div>
      ) : !account ? (
        <div className="space-y-3 py-6 text-sm">
          <p>
            Connect {provider === 'gitlab' ? 'GitLab' : 'GitHub'} to browse and
            deploy your repositories.
          </p>
          <Button asChild>
            <Link to="/git-providers/add">
              Connect {provider === 'gitlab' ? 'GitLab' : 'GitHub'}
            </Link>
          </Button>
          <Button asChild variant="ghost">
            <Link to="/git-providers">Git settings</Link>
          </Button>
          <p className="text-muted-foreground">
            Already signed in on the Temps host? Supported host credentials are
            imported when Temps starts.
          </p>
        </div>
      ) : (
        <>
          <div className="flex min-w-0 items-center gap-2">
            <Label className="sr-only" htmlFor="connected-git-account">
              Git account
            </Label>
            <Select
              value={String(account.id)}
              onValueChange={(value) =>
                update({
                  gitConnection: value,
                  repoSearch: null,
                  repoPage: null,
                })
              }
            >
              <SelectTrigger
                id="connected-git-account"
                className="min-w-0 flex-1"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {accounts.map((c) => (
                  <SelectItem key={c.id} value={String(c.id)}>
                    {c.account_name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Button variant="ghost" size="sm" asChild className="shrink-0">
              <Link to="/git-providers">
                <Settings2 className="size-4" />
                Settings
              </Link>
            </Button>
          </div>
          {!ready ? (
            <div role="alert" className="space-y-2 text-sm">
              <p>This Git connection needs attention before you can deploy.</p>
              <Button asChild variant="outline">
                <Link
                  to={`/git-providers/${account.provider_id}/connections/${account.id}`}
                >
                  Manage connection
                </Link>
              </Button>
            </div>
          ) : (
            <>
              <div className="flex gap-2">
                <div className="relative min-w-0 flex-1">
                  <Search
                    className="pointer-events-none absolute left-3 top-3 size-4 text-muted-foreground"
                    aria-hidden="true"
                  />
                  <Input
                    aria-label="Search repositories"
                    placeholder="Search repositories…"
                    className="pl-9"
                    value={search}
                    onChange={(e) =>
                      update(
                        { repoSearch: e.target.value, repoPage: null },
                        true
                      )
                    }
                  />
                </div>
                <Button
                  variant="outline"
                  aria-expanded={filtersOpen}
                  aria-controls="repository-filters"
                  onClick={() => setFiltersOpen(!filtersOpen)}
                >
                  <SlidersHorizontal className="size-4" />
                  Filters
                  {filterCount > 0 && (
                    <Badge variant="secondary">{filterCount}</Badge>
                  )}
                </Button>
              </div>
              {filtersOpen && (
                <div
                  id="repository-filters"
                  className="space-y-2 rounded-md border p-3"
                >
                  <div className="grid gap-3 sm:grid-cols-2">
                    <div className="space-y-1">
                      <Label htmlFor="repository-preset-filter">
                        Detected preset
                      </Label>
                      <Select
                        value={preset || '__all__'}
                        onValueChange={(value) =>
                          update({
                            repoPreset: value === '__all__' ? null : value,
                            repoPage: null,
                          })
                        }
                      >
                        <SelectTrigger id="repository-preset-filter">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="__all__">All presets</SelectItem>
                          <SelectItem value="__undetected__">
                            Not detected yet
                          </SelectItem>
                          {presetCatalog.data?.presets.map((item) => (
                            <SelectItem key={item.slug} value={item.slug}>
                              {item.label}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>
                    <div className="space-y-1">
                      <Label htmlFor="repository-updated-filter">
                        Last updated
                      </Label>
                      <Select
                        value={updatedDays || '__all__'}
                        onValueChange={(value) =>
                          update({
                            repoUpdated: value === '__all__' ? null : value,
                            repoPage: null,
                          })
                        }
                      >
                        <SelectTrigger id="repository-updated-filter">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="__all__">Any time</SelectItem>
                          <SelectItem value="7">Last 7 days</SelectItem>
                          <SelectItem value="30">Last 30 days</SelectItem>
                          <SelectItem value="90">Last 90 days</SelectItem>
                        </SelectContent>
                      </Select>
                    </div>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    Preset filters use saved detection results. Repositories are
                    scanned when you configure them.
                  </p>
                  {presetCatalog.isError && (
                    <p role="alert" className="text-sm">
                      Could not load preset choices.{' '}
                      <button
                        type="button"
                        className="underline"
                        onClick={() => void presetCatalog.refetch()}
                      >
                        Retry preset choices
                      </button>
                    </p>
                  )}
                  {filterCount > 0 && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() =>
                        update({
                          repoPreset: null,
                          repoUpdated: null,
                          repoPage: null,
                        })
                      }
                    >
                      Clear filters
                    </Button>
                  )}
                </div>
              )}
              {account.syncing && (
                <p role="status" className="text-sm text-muted-foreground">
                  Syncing repositories… {account.synced_repository_count} found.
                </p>
              )}
              {account.last_sync_error && (
                <p role="alert" className="text-sm text-destructive">
                  Repository sync failed: {account.last_sync_error}{' '}
                  <Link
                    className="underline"
                    to={`/git-providers/${account.provider_id}/connections/${account.id}`}
                  >
                    Manage connection
                  </Link>
                </p>
              )}
              {repos.isError ? (
                <div role="alert" className="space-y-2 text-sm">
                  <p>
                    Could not load repositories. Check your connection or retry.
                  </p>
                  <Button
                    variant="outline"
                    onClick={() => void repos.refetch()}
                  >
                    Retry repositories
                  </Button>
                </div>
              ) : repos.isPending || search !== debouncedSearch ? (
                <div
                  role="status"
                  aria-label="Loading repositories"
                  className="space-y-3"
                >
                  <Skeleton className="h-12 w-full" />
                  <Skeleton className="h-12 w-full" />
                </div>
              ) : (
                <>
                  <div className="divide-y border-y">
                    {repos.data.repositories.map((repo) => (
                      <Link
                        key={repo.id}
                        to={`/projects/import/${repo.id}`}
                        aria-label={`Configure ${repo.full_name}`}
                        className="flex min-w-0 items-center gap-3 py-3 hover:bg-muted/50 focus-visible:ring-2 focus-visible:ring-ring"
                      >
                        <div className="min-w-0 flex-1">
                          <p className="break-all text-sm font-medium">
                            {repo.full_name}
                          </p>
                          <p className="text-xs text-muted-foreground">
                            {repo.default_branch}
                            {repo.language ? ` · ${repo.language}` : ''}
                            {isValid(new Date(repo.updated_at)) && (
                              <>
                                {' '}
                                ·{' '}
                                <time
                                  dateTime={repo.updated_at}
                                  title={new Date(
                                    repo.updated_at
                                  ).toLocaleString()}
                                >
                                  Updated{' '}
                                  {formatDistanceToNow(
                                    new Date(repo.updated_at),
                                    { addSuffix: true }
                                  )}
                                </time>
                              </>
                            )}
                          </p>
                          <div
                            className="mt-1 flex flex-wrap gap-1"
                            aria-label="Detected presets"
                          >
                            {repo.preset?.length ? (
                              Array.from(
                                new Map(
                                  repo.preset.map((item) => [item.preset, item])
                                ).values()
                              ).map((item) => (
                                <Badge key={item.preset} variant="secondary">
                                  {item.presetLabel}
                                </Badge>
                              ))
                            ) : (
                              <span className="text-xs text-muted-foreground">
                                {repo.preset
                                  ? 'No preset detected'
                                  : 'Preset not detected yet'}
                              </span>
                            )}
                          </div>
                        </div>
                        <Badge variant="outline">
                          {repo.private ? 'Private' : 'Public'}
                        </Badge>
                        <ArrowRight className="size-4 shrink-0 text-muted-foreground" />
                      </Link>
                    ))}
                    {!repos.data.repositories.length && (
                      <div role="status" className="space-y-2 py-6 text-sm">
                        <p>
                          {search || preset || updatedDays
                            ? 'No matching repositories.'
                            : account.syncing
                              ? 'Your repositories will appear as syncing completes.'
                              : 'No repositories available for this account.'}
                        </p>
                        {(search || preset || updatedDays || page > 1) && (
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() =>
                              update({
                                repoSearch: null,
                                repoPage: null,
                                repoPreset: null,
                                repoUpdated: null,
                              })
                            }
                          >
                            Reset search
                          </Button>
                        )}
                      </div>
                    )}
                  </div>
                  <ResponsivePagination
                    ariaLabel="Repository pagination"
                    page={page}
                    pageSize={pageSize}
                    total={total}
                    totalPages={pages}
                    onPageChange={(nextPage) =>
                      update({ repoPage: String(nextPage) })
                    }
                  />
                </>
              )}
            </>
          )}
        </>
      )}
      <div className="border-t pt-3">
        <Button variant="ghost" size="sm" asChild>
          <Link to="/projects/new">
            More ways to import a repository <ArrowRight className="size-4" />
          </Link>
        </Button>
      </div>
    </section>
  )
}
