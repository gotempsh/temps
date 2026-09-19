// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { PageHeader, Status } from '@temps-sdk/ds'
import { listRepositoryPluginCatalog } from '@/api/client/sdk.gen'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { RepositoryInstall } from '@/components/plugins/RepositoryInstall'
import { useAuth } from '@/contexts/AuthContext'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { canManageExternalPlugins } from '@/lib/plugin-registry'
import {
  ArrowLeft,
  GitBranch,
  GitCommitHorizontal,
  Tag,
  Folder,
  Monitor,
  BookOpen,
  Code,
  User,
} from 'lucide-react'

export function PluginInstallPage() {
  const [params] = useSearchParams()
  const name = params.get('plugin')
  const commit = params.get('commit')
  const navigate = useNavigate()
  const { user } = useAuth()
  const canInstall = canManageExternalPlugins(user?.role)
  const { setBreadcrumbs } = useBreadcrumbs()
  const [pending, setPending] = useState(false)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const catalog = useQuery({
    queryKey: ['repository-plugin-catalog'],
    queryFn: async () =>
      (await listRepositoryPluginCatalog({ throwOnError: true })).data,
    enabled: Boolean(name),
    staleTime: 15 * 60 * 1000,
    retry: false,
  })
  const plugin = catalog.data?.plugins.find((p) => p.name === name)
  const changed = Boolean(plugin && commit && plugin.commit !== commit)
  usePageTitle(plugin ? `Install ${plugin.title}` : 'Install plugin')
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Plugins', href: '/settings/plugins' },
      { label: 'Review & install' },
    ])
  }, [setBreadcrumbs])
  const source = plugin
    ? `${plugin.repository}/tree/${plugin.commit}${plugin.path ? '/' + plugin.path.split('/').map(encodeURIComponent).join('/') : ''}`
    : null
  return (
    <div className="w-full min-w-0 space-y-6">
      <Button asChild variant="ghost" size="sm" className="-ml-3">
        <Link to="/settings/plugins">
          <ArrowLeft className="mr-2 size-4" />
          Back to plugins
        </Link>
      </Button>
      <PageHeader
        title={plugin ? plugin.title : 'Install from GitHub'}
        description={
          plugin
            ? plugin.summary
            : 'Review a plugin’s source and choose its access to your instance.'
        }
      />
      {name && catalog.isPending ? (
        <Skeleton className="h-64 w-full" />
      ) : name && (!plugin || !catalog.data?.available || catalog.isError) ? (
        <section role="alert" className="space-y-3 rounded-lg border p-5">
          <h2 className="font-semibold">Plugin details unavailable</h2>
          <p className="text-sm text-muted-foreground">
            This plugin may no longer be listed, or the catalog could not be
            loaded. Return to the catalog or try again.
          </p>
          <Button variant="outline" onClick={() => void catalog.refetch()}>
            Retry
          </Button>
        </section>
      ) : (
        <div className="min-w-0 space-y-6">
          <div className="min-w-0 space-y-6">
            {plugin && (
              <>
                <section
                  className="space-y-4"
                  aria-label="Source and compatibility"
                >
                  <div className="flex flex-wrap items-center gap-2">
                    <Badge variant="outline">
                      <Tag className="mr-1.5 size-3.5" aria-hidden="true" />v
                      {plugin.latestVersion}
                    </Badge>
                    <Badge variant="outline">
                      <GitBranch
                        className="mr-1.5 size-3.5"
                        aria-hidden="true"
                      />
                      {plugin.ref || 'Default branch'}
                    </Badge>
                    <a
                      href={source!}
                      target="_blank"
                      rel="noopener noreferrer"
                      title={`Pinned commit: ${plugin.commit}`}
                      aria-label={`Pinned commit ${plugin.commit}`}
                    >
                      <Badge variant="outline">
                        <GitCommitHorizontal
                          className="mr-1.5 size-3.5"
                          aria-hidden="true"
                        />
                        <span className="font-mono">
                          {plugin.commit.slice(0, 8)}
                        </span>
                      </Badge>
                    </a>
                    <Status
                      tone={
                        plugin.validation.build === 'passed' ? 'ok' : 'idle'
                      }
                      label={
                        plugin.validation.build === 'passed'
                          ? 'Build passed'
                          : 'Build not verified'
                      }
                    />
                    <Status
                      tone={
                        plugin.validation.metadata === 'passed' ? 'ok' : 'idle'
                      }
                      label={
                        plugin.validation.metadata === 'passed'
                          ? 'Metadata checked'
                          : 'Metadata not verified'
                      }
                    />
                    {plugin.platforms.map((platform) => (
                      <Badge variant="outline" key={platform}>
                        <Monitor
                          className="mr-1.5 size-3.5"
                          aria-hidden="true"
                        />
                        {platform}
                      </Badge>
                    ))}
                    {plugin.path && (
                      <Badge variant="outline" className="break-all">
                        <Folder
                          className="mr-1.5 size-3.5"
                          aria-hidden="true"
                        />
                        {plugin.path}
                      </Badge>
                    )}
                  </div>
                  <div className="flex flex-wrap items-center gap-x-5 gap-y-2 text-sm text-muted-foreground">
                    <span className="inline-flex items-center gap-1.5">
                      <User className="size-4" aria-hidden="true" />
                      {plugin.author}
                    </span>
                    <span>{plugin.category}</span>
                    {[
                      { label: 'Source code', url: source, Icon: Code },
                      {
                        label: 'Documentation',
                        url: plugin.docsUrl,
                        Icon: BookOpen,
                      },
                      {
                        label: 'README',
                        url: plugin.readmeUrl,
                        Icon: BookOpen,
                      },
                    ].map(({ label, url, Icon }) =>
                      url && /^https:\/\//.test(url) ? (
                        <a
                          key={label}
                          href={url}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="inline-flex items-center gap-1.5 underline underline-offset-4 hover:text-foreground"
                        >
                          <Icon className="size-4" aria-hidden="true" />
                          {label}
                        </a>
                      ) : null
                    )}
                  </div>
                  {plugin.description &&
                    plugin.description !== plugin.summary && (
                      <p className="whitespace-pre-wrap text-sm leading-relaxed text-muted-foreground">
                        {plugin.description}
                      </p>
                    )}
                  <p className="text-xs text-muted-foreground">
                    Installs the pinned commit shown above. Catalog checks are
                    not a security audit.
                  </p>
                </section>
                {plugin.screenshots.length > 0 && (
                  <section
                    aria-label="Plugin screenshots"
                    className="grid gap-4 sm:grid-cols-2"
                  >
                    {plugin.screenshots.map((shot, i) => (
                      <figure key={i} className="space-y-2">
                        <img
                          src={shot.url}
                          alt={shot.alt}
                          loading="lazy"
                          referrerPolicy="no-referrer"
                          className="aspect-video w-full rounded-lg border object-contain"
                        />
                        {shot.caption && (
                          <figcaption className="text-sm text-muted-foreground">
                            {shot.caption}
                          </figcaption>
                        )}
                      </figure>
                    ))}
                  </section>
                )}
              </>
            )}
          </div>
          <div className="min-w-0 space-y-4">
            {changed ? (
              <section role="alert" className="space-y-3 rounded-lg border p-5">
                <h2 className="font-semibold">
                  The catalog revision has changed
                </h2>
                <p className="text-sm text-muted-foreground">
                  Return to the catalog to review the current version before
                  installing.
                </p>
                <Button asChild variant="outline">
                  <Link to="/settings/plugins">Review current catalog</Link>
                </Button>
              </section>
            ) : canInstall ? (
              <section className="rounded-lg border bg-card p-5">
                <RepositoryInstall
                  compact
                  disabled={pending}
                  selection={plugin}
                  onSensitiveError={handleSensitiveActionError}
                  onPendingChange={setPending}
                  onClearSelection={() => navigate('/settings/plugins')}
                  onInstalled={() => navigate('/settings/plugins')}
                />
              </section>
            ) : (
              <p className="rounded-lg border p-5 text-sm">
                A system administrator can install this plugin.
              </p>
            )}
          </div>
        </div>
      )}
      {canInstall && verificationDialog}
    </div>
  )
}
