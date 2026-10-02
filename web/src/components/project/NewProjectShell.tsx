// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type ComponentType, type ReactNode } from 'react'
import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import {
  listConnectionsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@/components/ui/tabs'
import { Badge } from '@/components/ui/badge'
import { FeatureMaturityBadge } from '@/components/feature-maturity/FeatureMaturityBadge'
import { ProviderLogo } from '@/components/git/ProviderLogo'
import {
  Link as LinkIcon,
  LayoutTemplate,
  Container,
  FolderGit2,
  CheckCircle2,
  UploadCloud,
  Boxes,
  Activity,
} from 'lucide-react'
import Github from '@/icons/Github'
import Gitlab from '@/icons/Gitlab'
import Gitea from '@/icons/Gitea'

export type ProjectSource =
  | 'templates'
  | 'services'
  | 'browse'
  | 'git-url'
  | 'manual'
  | 'drop'
  | 'monitor'

/**
 * Shared page shell for every step of project creation — the source picker,
 * the repository configurator, the template configurator, and the Git URL
 * flow all render inside the same header (title + connection chips) and
 * source-tab row, so switching between "choosing" and "configuring" never
 * swaps the page's frame out from under the user.
 */
export function NewProjectShell({
  activeSource,
  onSelectSource,
  children,
}: {
  activeSource: ProjectSource | null
  onSelectSource: (source: ProjectSource) => void
  children: ReactNode
}) {
  const { data: connections } = useQuery({ ...listConnectionsOptions() })
  const { data: gitProviders } = useQuery({ ...listGitProvidersOptions() })

  const providerTypeForConnectionId = (
    providerId: number
  ): string | undefined =>
    gitProviders?.find((p) => p.id === providerId)?.provider_type

  const connectionCount = connections?.connections?.length ?? 0

  // The repositories tab carries the connected provider's identity (icon +
  // name) so the tab reads "GitHub", not a generic "Import Repository".
  // With connections to several different providers (or none, or a type we
  // don't have branding for) it stays a neutral "Repositories" — the account
  // dropdown inside the panel is where you switch between providers.
  const PROVIDER_TABS: Record<
    string,
    { icon: ComponentType<{ className?: string }>; title: string }
  > = {
    github: { icon: Github, title: 'GitHub' },
    gitlab: { icon: Gitlab, title: 'GitLab' },
    gitea: { icon: Gitea, title: 'Gitea' },
    generic: { icon: FolderGit2, title: 'Git' },
  }
  const normalizedProviderTypes = new Set(
    (connections?.connections ?? [])
      .map((c) => providerTypeForConnectionId(c.provider_id))
      .filter((t): t is string => Boolean(t))
      .map((t) => (t === 'github_app' ? 'github' : t))
  )
  const soleProviderType =
    normalizedProviderTypes.size === 1
      ? [...normalizedProviderTypes][0]
      : undefined
  const browseTab = (soleProviderType && PROVIDER_TABS[soleProviderType]) || {
    icon: FolderGit2,
    title: 'Repositories',
  }

  const sources: Array<{
    key: ProjectSource
    icon: ComponentType<{ className?: string }>
    title: string
    featureKey?: string
  }> = [
    { key: 'browse', icon: browseTab.icon, title: browseTab.title },
    { key: 'templates', icon: LayoutTemplate, title: 'Template' },
    {
      key: 'services',
      icon: Boxes,
      title: 'Services',
      featureKey: 'service-template-catalog',
    },
    { key: 'git-url', icon: LinkIcon, title: 'Git URL' },
    { key: 'manual', icon: Container, title: 'Docker Image' },
    { key: 'drop', icon: UploadCloud, title: 'Drop files' },
    {
      key: 'monitor',
      icon: Activity,
      title: 'Monitor an existing application',
    },
  ]

  return (
    <div className="flex-1 min-w-0">
      <div className="mb-6 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">New Project</h1>
          <p className="text-sm text-muted-foreground mt-1">
            Deploy your code or monitor an application hosted elsewhere.
          </p>
        </div>
        {activeSource !== 'monitor' && (
          <div className="flex flex-wrap items-center gap-2">
            {connectionCount > 0 ? (
              connections!.connections.slice(0, 3).map((conn) => (
                <span
                  key={conn.id}
                  className="inline-flex items-center gap-2 rounded-full border py-1 pr-1 pl-3 text-sm"
                >
                  <ProviderLogo
                    providerType={providerTypeForConnectionId(conn.provider_id)}
                    className="h-4 w-4 shrink-0"
                  />
                  <span className="max-w-40 truncate">{conn.account_name}</span>
                  <Badge variant="secondary" className="gap-1 py-0.5 pr-2 pl-1">
                    <CheckCircle2 className="h-3 w-3" />
                    Connected
                  </Badge>
                </span>
              ))
            ) : (
              <span className="text-sm text-muted-foreground">
                No Git provider connected
              </span>
            )}
            <Link
              to="/git-providers"
              className="text-xs text-muted-foreground underline underline-offset-2 hover:text-foreground transition-colors"
            >
              Manage
            </Link>
          </div>
        )}
      </div>

      <Tabs
        value={activeSource ?? ''}
        onValueChange={(value) => onSelectSource(value as ProjectSource)}
      >
        <TabsList aria-label="Project setup">
          {sources.map((source) => {
            const Icon = source.icon
            return (
              <TabsTrigger key={source.key} value={source.key}>
                <Icon className="h-4 w-4 shrink-0" aria-hidden="true" />
                {source.title}
                <FeatureMaturityBadge featureKey={source.featureKey} compact />
              </TabsTrigger>
            )
          })}
        </TabsList>
        <TabsContent value={activeSource ?? ''}>{children}</TabsContent>
      </Tabs>
    </div>
  )
}
