// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Blocks,
  Boxes,
  CodeXml,
  Container,
  Flag,
  GitBranch,
  KeyRound,
  LockKeyhole,
  Plug,
  PlugZap,
  Puzzle,
  Rocket,
  Settings2,
  Webhook,
  Workflow,
} from 'lucide-react'
import type { ReactNode } from 'react'
import { PageHeader, SettingsSurface } from '@temps-sdk/ds'
import type { LucideIcon } from 'lucide-react'
import type { ProjectResponse } from '@/api/client'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  settingsSectionDomId,
  type CombinedSettingsPage,
  type SettingsSectionId,
} from '@/lib/project-settings-sections'
import { AutomationSettings } from './AutomationSettings'
import { GeneralSettings } from './GeneralSettings'
import { GitSettings } from './GitSettings'
import { BuildDeploySettings } from './BuildDeploySettings'
import { SecretsSettings } from './SecretsSettings'
import { DeploymentTokensSettings } from './DeploymentTokensSettings'
import { WebhooksSettings } from './WebhooksSettings'
import { SkillsSettings } from './SkillsSettings'
import { McpServersSettings } from './McpServersSettings'
import {
  HostDockerAccessAlert,
  HostDockerAccessOnboarding,
  useHostDockerAccess,
} from '@/components/project/HostDockerAccessAlert'
import { ProjectFeatureFlags } from '@/components/project/flags/ProjectFeatureFlags'
import { Link } from 'react-router'
import { usePluginsContext } from '@/contexts/PluginsContext'
import { useConsoleExtensions } from '@temps-sdk/console-kit'

export type { CombinedSettingsPage }
const titles: Record<CombinedSettingsPage, string> = {
  general: 'General',
  delivery: 'Build & deploy',
  variables: 'Secrets',
  'deployment-tokens': 'Deployment tokens',
  automation: 'Automation',
  integrations: 'Integrations',
}

/** One line under each page title, so every Configure page opens the same way. */
const descriptions: Record<CombinedSettingsPage, string> = {
  general: 'How this service is named, what it reports, and deleting it.',
  delivery: 'Where the code comes from, how it is built, and how it ships.',
  variables: '',
  'deployment-tokens': '',
  automation:
    'Jobs the AI does on its own. Every run is a chat you can open and continue.',
  integrations:
    'Webhooks, skills, MCP servers and extensions for this service.',
}

type Section = {
  id: SettingsSectionId<CombinedSettingsPage>
  title: string
  /** One line under the title in the left column. */
  description?: string
  icon: LucideIcon
  content: ReactNode
  /** The content lays out its own groups under the page title; no heading. */
  headless?: boolean
  /**
   * The content draws the page header itself (a list page with its primary
   * action on the right), so the page adds no title of its own.
   */
  ownsPageHeader?: boolean
}

/**
 * Related forms share a URL. Disclosure sections reduce scrolling without
 * another navigation level, and `?section=<id>` opens one and scrolls to it so
 * links, the command palette and redirects from the old standalone pages can
 * land on the exact setting.
 */
export function CombinedProjectSettings({
  page,
  project,
  refetch,
  intro,
}: {
  page: CombinedSettingsPage
  project: ProjectResponse
  refetch: () => void
  /** Rendered between the page title and the first section. */
  intro?: ReactNode
}) {
  usePageTitle(`${titles[page]} · ${project.name}`)
  const hostDockerAccess = useHostDockerAccess(project)

  let sections: Section[]
  // A project holding host Docker access is root-equivalent on its hosts:
  // say so before anything else. For everyone else the grant is an operator
  // detail and lives in the collapsed Advanced section at the bottom.
  const banner =
    page === 'delivery' && hostDockerAccess.placement === 'prominent' ? (
      <HostDockerAccessAlert
        project={project}
        canManageNodes={hostDockerAccess.canManageNodes}
      />
    ) : null
  switch (page) {
    case 'general':
      sections = [
        {
          id: 'project',
          title: 'Service settings',
          icon: Settings2,
          content: <GeneralSettings project={project} refetch={refetch} />,
          headless: true,
        },
      ]
      break
    case 'delivery':
      sections = [
        {
          id: 'source',
          title: 'Source',
          description: 'Whether Temps runs this app, and what it deploys from.',
          icon: CodeXml,
          content: (
            <BuildDeploySettings
              project={project}
              refetch={refetch}
              section="source"
            />
          ),
        },
        {
          id: 'repository',
          title: 'Repository',
          description: 'The Git repository and branch Temps builds from.',
          icon: GitBranch,
          content: <GitSettings project={project} refetch={refetch} embedded />,
        },
        {
          id: 'build',
          title: 'Build',
          description: 'How the code is turned into an image.',
          icon: Container,
          content: (
            <BuildDeploySettings
              project={project}
              refetch={refetch}
              section="build"
            />
          ),
        },
        {
          id: 'deployment',
          title: 'Deployment',
          description:
            'Defaults every environment starts from: ports, health checks, resources.',
          icon: Rocket,
          content: (
            <BuildDeploySettings
              project={project}
              refetch={refetch}
              section="deploy"
            />
          ),
        },
        {
          id: 'previews',
          title: 'Previews',
          description:
            'Short-lived environments for branches and pull requests.',
          icon: Blocks,
          content: (
            <BuildDeploySettings
              project={project}
              refetch={refetch}
              section="previews"
            />
          ),
        },
        {
          id: 'feature-flags',
          title: 'Feature flags',
          description: 'Turn features on and off without a deploy.',
          icon: Flag,
          content: <ProjectFeatureFlags project={project} />,
        },
      ]
      if (hostDockerAccess.placement === 'advanced') {
        sections.push({
          id: 'host-access',
          title: 'Advanced: host Docker access',
          description:
            'Give this service the Docker socket on the hosts that run it.',
          icon: Plug,
          content: <HostDockerAccessOnboarding project={project} />,
        })
      }
      break
    case 'variables':
      sections = [
        {
          id: 'secrets',
          title: 'Secrets',
          icon: LockKeyhole,
          content: <SecretsSettings project={project} />,
          headless: true,
          ownsPageHeader: true,
        },
      ]
      break
    case 'deployment-tokens':
      sections = [
        {
          id: 'deployment-tokens',
          title: 'Deployment tokens',
          icon: KeyRound,
          content: <DeploymentTokensSettings project={project} />,
          headless: true,
          ownsPageHeader: true,
        },
      ]
      break
    case 'automation':
      sections = [
        {
          id: 'automations',
          title: 'Automation',
          icon: Workflow,
          content: <AutomationSettings project={project} />,
          headless: true,
        },
      ]
      break
    case 'integrations':
      sections = [
        {
          id: 'webhooks',
          title: 'Webhooks',
          description: 'Send deployment and error events to other systems.',
          icon: Webhook,
          content: <WebhooksSettings project={project} />,
        },
        {
          id: 'skills',
          title: 'Skills',
          description:
            'Instructions the AI loads when it works on this service.',
          icon: Puzzle,
          content: <SkillsSettings project={project} />,
        },
        {
          id: 'mcp-servers',
          title: 'MCP servers',
          description: 'Extra tools the AI can call for this service.',
          icon: PlugZap,
          content: <McpServersSettings project={project} />,
        },
        {
          id: 'extensions',
          title: 'Extensions',
          description: 'Pages added by installed plugins.',
          icon: Boxes,
          content: <ProjectExtensionLinks project={project} />,
        },
      ]
      break
  }
  // Every section is always open and laid out the same way on every page: a
  // group with its title and purpose on the left and its settings on the
  // right, groups separated by a rule. Never a card around a section.
  const single = sections.length === 1 && sections[0].headless
  return (
    <div className="min-w-0 space-y-6">
      {!sections.some((section) => section.ownsPageHeader) && (
        <PageHeader
          title={titles[page]}
          description={descriptions[page] || undefined}
        />
      )}
      {banner}
      {single ? (
        <section
          id={settingsSectionDomId(sections[0].id)}
          aria-label={sections[0].title}
          className="min-w-0 scroll-mt-4"
        >
          {sections[0].content}
        </section>
      ) : (
        <div className="min-w-0 divide-y [&>section]:py-8 [&>section:first-child]:pt-0">
          {intro}
          {sections.map((section) => (
            <SettingsBlock
              key={`${page}-${section.id}`}
              id={settingsSectionDomId(section.id)}
              title={section.title}
              description={section.description}
            >
              {section.content}
            </SettingsBlock>
          ))}
        </div>
      )}
    </div>
  )
}

/**
 * One always-open group of settings, laid out like `SettingsGroup`: what it
 * is on the left, the settings on the right. The content is marked as being
 * on a settings surface, so cards and sections inside it render flat instead
 * of as a box in a box.
 */
function SettingsBlock({
  id,
  title,
  description,
  children,
}: {
  id: string
  title: string
  description?: string
  children: ReactNode
}) {
  const headingId = `${id}-heading`
  return (
    <section
      id={id}
      aria-labelledby={headingId}
      className="grid min-w-0 scroll-mt-4 gap-5 md:grid-cols-[minmax(0,1fr)_minmax(0,2fr)] md:gap-10"
    >
      <div className="min-w-0">
        <h2 id={headingId} className="text-base font-semibold">
          {title}
        </h2>
        {description && (
          <p className="mt-1 text-sm text-muted-foreground">{description}</p>
        )}
      </div>
      <div className="min-w-0 space-y-5">
        <SettingsSurface>{children}</SettingsSurface>
      </div>
    </section>
  )
}

function ProjectExtensionLinks({ project }: { project: ProjectResponse }) {
  const { projectNavEntries } = usePluginsContext()
  const { projectToolLinks } = useConsoleExtensions()
  const links = [
    ...projectNavEntries.map((entry) => ({
      title: entry.label,
      href: entry.path.startsWith('/')
        ? entry.path
        : `/projects/${project.slug}/${entry.path}`,
    })),
    ...(projectToolLinks ?? []).map((entry) => ({
      title: entry.title,
      href: entry.href(project),
    })),
  ]
  return links.length ? (
    <div className="space-y-2">
      {links.map((link) => (
        <Link
          key={link.href}
          to={link.href}
          className="block rounded-md border px-3 py-2 text-sm hover:bg-muted"
        >
          {link.title}
        </Link>
      ))}
    </div>
  ) : (
    <p className="text-sm text-muted-foreground">
      Installed service extensions will appear here.
    </p>
  )
}
