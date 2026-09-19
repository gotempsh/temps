// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Search, RefreshCw } from 'lucide-react'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useQuery } from '@tanstack/react-query'
import { listRepositoryPluginCatalog } from '@/api/client/sdk.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import type { RepositorySelection } from './RepositoryInstall'
import { filterRepositoryCatalog } from './RepositoryCatalog-shared'

export function RepositoryCatalog({
  canInstall,
  disabled,
  installedNames,
  onSelect,
}: {
  canInstall: boolean
  disabled: boolean
  installedNames: string[]
  onSelect: (plugin: RepositorySelection) => void
}) {
  const [search, setSearch] = useState('')
  const [category, setCategory] = useState('')
  const [limit, setLimit] = useState(24)
  const catalog = useQuery({
    queryKey: ['repository-plugin-catalog'],
    queryFn: async () =>
      (await listRepositoryPluginCatalog({ throwOnError: true })).data,
    staleTime: 15 * 60 * 1000,
    retry: false,
    refetchOnWindowFocus: false,
  })
  const plugins = catalog.data?.plugins ?? []
  const categories = [
    ...new Set(plugins.map((plugin) => plugin.category)),
  ].sort()
  const filtered = filterRepositoryCatalog(plugins, search, category)
  return (
    <section className="space-y-4" aria-labelledby="repository-catalog-title">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
        <h2 id="repository-catalog-title" className="sr-only">
          Available plugins
        </h2>
        <div className="relative min-w-0 flex-1">
          <Search
            className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
            aria-hidden="true"
          />
          <Input
            type="search"
            name="plugin-catalog-search"
            aria-label="Search available plugins"
            className="pl-9"
            placeholder="Search plugins…"
            value={search}
            onChange={(event) => {
              setSearch(event.target.value)
              setLimit(24)
            }}
          />
        </div>
        <div className="flex items-center gap-2">
          <Select
            value={category || 'all'}
            onValueChange={(value) => {
              setCategory(value === 'all' ? '' : value)
              setLimit(24)
            }}
          >
            <SelectTrigger
              className="w-full sm:w-44"
              aria-label="Plugin category"
            >
              <SelectValue placeholder="All categories" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All categories</SelectItem>
              {categories.map((value) => (
                <SelectItem key={value} value={value}>
                  {value}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button
            type="button"
            variant="outline"
            size="icon"
            aria-label="Refresh catalog"
            title="Refresh catalog"
            disabled={catalog.isFetching}
            onClick={() => void catalog.refetch()}
          >
            <RefreshCw
              className={`size-4 ${catalog.isFetching ? 'animate-spin' : ''}`}
            />
          </Button>
        </div>
      </div>
      {catalog.isPending ? (
        <div
          className="grid gap-3 sm:grid-cols-2"
          aria-label="Loading plugin catalog"
        >
          <Skeleton className="h-40 rounded-lg" />
          <Skeleton className="h-40 rounded-lg" />
        </div>
      ) : catalog.isError || !catalog.data?.available ? (
        <div role="status" className="space-y-2 rounded-lg border p-4">
          <p className="font-medium">GitHub catalog unavailable</p>
          <p className="text-sm text-muted-foreground">
            {catalog.data?.reason ||
              'Could not fetch the catalog. Try refreshing it again.'}
          </p>
          <p className="text-sm">
            <a
              className="underline underline-offset-4"
              href="https://github.com/gotempsh/plugins/tree/main/registry"
              target="_blank"
              rel="noopener noreferrer"
            >
              View catalog on GitHub
            </a>
          </p>
        </div>
      ) : (
        <>
          {!canInstall && (
            <p className="text-sm text-muted-foreground">
              A system administrator can install these plugins.
            </p>
          )}
          <p
            role="status"
            className="text-sm text-muted-foreground tabular-nums"
          >
            {filtered.length} {filtered.length === 1 ? 'plugin' : 'plugins'}{' '}
            found.
          </p>
          {filtered.length === 0 ? (
            <div className="rounded-lg border p-4 text-sm text-muted-foreground">
              {catalog.data?.plugins.length === 0 ? (
                <>
                  <p>No catalog plugins support this server’s platform yet.</p>
                  <p>
                    Compatible plugins will appear here when they are listed.
                  </p>
                </>
              ) : (
                <p>No matching plugins. Try another search or category.</p>
              )}
            </div>
          ) : (
            <div className="divide-y rounded-lg border bg-card">
              {filtered.slice(0, limit).map((plugin) => (
                <article
                  key={plugin.name}
                  className="flex min-w-0 flex-col gap-4 p-4 sm:flex-row sm:items-center sm:gap-6 sm:p-5"
                >
                  <div className="flex min-w-0 flex-1 items-start gap-3">
                    <Avatar className="size-10 shrink-0 rounded-md">
                      {plugin.logoUrl && (
                        <AvatarImage
                          src={plugin.logoUrl}
                          alt=""
                          loading="lazy"
                          referrerPolicy="no-referrer"
                        />
                      )}
                      <AvatarFallback className="rounded-md">
                        {plugin.title.slice(0, 2).toUpperCase()}
                      </AvatarFallback>
                    </Avatar>
                    <div className="min-w-0">
                      <h3 className="break-words font-semibold">
                        {plugin.title}
                      </h3>
                      <p className="mt-1 text-pretty break-words text-sm text-muted-foreground">
                        {plugin.summary}
                      </p>
                      <p className="mt-2 text-xs text-muted-foreground">
                        {plugin.category} · By {plugin.author} · v
                        {plugin.latestVersion}
                      </p>
                    </div>
                  </div>
                  <div className="flex shrink-0 flex-wrap items-center gap-3">
                    <div className="text-sm">
                      <a
                        className="underline underline-offset-4"
                        href={`${plugin.repository}/tree/${plugin.commit}${plugin.path ? `/${plugin.path.split('/').map(encodeURIComponent).join('/')}` : ''}`}
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        Review source
                      </a>
                    </div>
                    {installedNames.includes(plugin.name) && (
                      <Badge variant="secondary">Installed</Badge>
                    )}
                    {canInstall && (
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        disabled={disabled}
                        onClick={() => onSelect(plugin)}
                      >
                        {installedNames.includes(plugin.name)
                          ? 'Review version'
                          : 'Review and install'}
                      </Button>
                    )}
                  </div>
                </article>
              ))}
            </div>
          )}
          {filtered.length > limit && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setLimit((value) => value + 24)}
            >
              Show more
            </Button>
          )}
          <p className="text-sm text-muted-foreground">
            {catalog.data?.platform && (
              <>
                Compatible with <code>{catalog.data.platform}</code>.{' '}
              </>
            )}
            Community listings are not a security audit; review access before
            installing.
          </p>
        </>
      )}
    </section>
  )
}
