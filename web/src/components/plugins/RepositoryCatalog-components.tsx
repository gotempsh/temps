// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
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
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <h2 id="repository-catalog-title" className="font-semibold">
            Available plugins
          </h2>
          <p className="text-base text-muted-foreground sm:text-sm">
            GitHub repositories listed by the community. Only plugins supporting
            this server’s platform are shown.
          </p>
          {catalog.data?.platform && (
            <p className="text-sm text-muted-foreground">
              Platform: <code>{catalog.data.platform}</code>
            </p>
          )}
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={catalog.isFetching}
          onClick={() => void catalog.refetch()}
        >
          {catalog.isFetching ? 'Checking…' : 'Refresh catalog'}
        </Button>
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
          <Input
            type="search"
            name="plugin-catalog-search"
            aria-label="Search available plugins"
            placeholder="Search plugins, authors, repositories…"
            value={search}
            onChange={(event) => {
              setSearch(event.target.value)
              setLimit(24)
            }}
          />
          <div className="flex flex-wrap gap-2" aria-label="Plugin categories">
            {['', ...categories].map((value) => (
              <Button
                type="button"
                key={value}
                variant="outline"
                size="sm"
                aria-pressed={category === value}
                className={
                  category === value
                    ? 'bg-accent text-accent-foreground hover:bg-accent hover:text-accent-foreground'
                    : ''
                }
                onClick={() => {
                  setCategory(value)
                  setLimit(24)
                }}
              >
                {value || 'All'}
              </Button>
            ))}
          </div>
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
            <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
              {filtered.slice(0, limit).map((plugin) => (
                <article
                  key={plugin.name}
                  className="flex min-w-0 flex-col gap-3 rounded-lg border p-4"
                >
                  <div className="flex items-start gap-3">
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
                      <p className="text-sm text-muted-foreground">
                        v{plugin.latestVersion} · {plugin.category}
                      </p>
                    </div>
                  </div>
                  <p className="text-pretty break-words text-base text-muted-foreground sm:text-sm">
                    {plugin.summary}
                  </p>
                  <p className="text-sm text-muted-foreground">
                    By {plugin.author}
                  </p>
                  <div className="mt-auto flex flex-wrap items-center justify-between gap-2 border-t pt-3">
                    <div className="text-sm">
                      <a
                        className="underline underline-offset-4"
                        href={`${plugin.repository}/tree/${plugin.commit}`}
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
            Listing and build checks are not a security audit. You must
            explicitly trust a repository before installation.
          </p>
        </>
      )}
    </section>
  )
}
