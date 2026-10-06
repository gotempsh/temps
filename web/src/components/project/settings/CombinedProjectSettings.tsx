// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { SettingsSection } from '@/components/ui/settings-section'
import {
  Blocks,
  Bot,
  Boxes,
  Braces,
  CalendarClock,
  CodeXml,
  Container,
  Flag,
  GitBranch,
  KeyRound,
  ListChecks,
  LockKeyhole,
  Plug,
  PlugZap,
  Puzzle,
  Rocket,
  Settings2,
  Sparkles,
  Webhook,
} from 'lucide-react'
import { useEffect, type ReactNode } from 'react'
import type { LucideIcon } from 'lucide-react'
import type { ProjectResponse } from '@/api/client'
import { usePageTitle } from '@/hooks/usePageTitle'
import { Button } from '@/components/ui/button'
import {
  requestedSettingsSection,
  settingsSectionDomId,
  type CombinedSettingsPage,
  type SettingsSectionId,
} from '@/lib/project-settings-sections'
import { GeneralSettings } from './GeneralSettings'
import { GitSettings } from './GitSettings'
import { BuildDeploySettings } from './BuildDeploySettings'
import { SecretsSettings } from './SecretsSettings'
import { DeploymentTokensSettings } from './DeploymentTokensSettings'
import { CronJobsSettings } from './CronJobsSettings'
import { WebhooksSettings } from './WebhooksSettings'
import { SkillsSettings } from './SkillsSettings'
import { McpServersSettings } from './McpServersSettings'
import {
  HostDockerAccessAlert,
  HostDockerAccessOnboarding,
  useHostDockerAccess,
} from '@/components/project/HostDockerAccessAlert'
import { ProjectFeatureFlags } from '@/components/project/flags/ProjectFeatureFlags'
import { AutopilotPage } from '@/components/agents/AutopilotPage'
import { AutofixerPage } from '@/components/autofixer/AutofixerPage'
import { ProjectSetup } from '@/pages/ProjectSetup'
import { Link, useSearchParams } from 'react-router'
import { usePluginsContext } from '@/contexts/PluginsContext'
import { useConsoleExtensions } from '@temps-sdk/console-kit'

export type { CombinedSettingsPage }
const titles: Record<CombinedSettingsPage, string> = {
  general: 'General',
  delivery: 'Build & deploy',
  variables: 'Variables & secrets',
  automation: 'Automation',
  integrations: 'Integrations',
}

type Section = {
  id: SettingsSectionId<CombinedSettingsPage>
  title: string
  icon: LucideIcon
  content: ReactNode
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
}: {
  page: CombinedSettingsPage
  project: ProjectResponse
  refetch: () => void
}) {
  usePageTitle(`${titles[page]} · ${project.name}`)
  const [searchParams] = useSearchParams()
  const activeSection = requestedSettingsSection(page, searchParams)
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
          title: 'Project settings',
          icon: Settings2,
          content: <GeneralSettings project={project} refetch={refetch} />,
        },
        {
          id: 'setup',
          title: 'Project setup',
          icon: ListChecks,
          content: <ProjectSetup project={project} />,
        },
      ]
      break
    case 'delivery':
      sections = [
        {
          id: 'source',
          title: 'Source',
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
          icon: GitBranch,
          content: <GitSettings project={project} refetch={refetch} embedded />,
        },
        {
          id: 'build',
          title: 'Build',
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
          icon: Flag,
          content: <ProjectFeatureFlags project={project} />,
        },
      ]
      if (hostDockerAccess.placement === 'advanced') {
        sections.push({
          id: 'host-access',
          title: 'Advanced: host Docker access',
          icon: Plug,
          content: <HostDockerAccessOnboarding project={project} />,
        })
      }
      break
    case 'variables':
      sections = [
        {
          id: 'environment-variables',
          title: 'Environment Variables',
          icon: Braces,
          content: <EnvironmentVariablesLink project={project} />,
        },
        {
          id: 'secrets',
          title: 'Secrets',
          icon: LockKeyhole,
          content: <SecretsSettings project={project} />,
        },
        {
          id: 'deployment-tokens',
          title: 'Deployment tokens',
          icon: KeyRound,
          content: <DeploymentTokensSettings project={project} />,
        },
      ]
      break
    case 'automation':
      sections = [
        {
          id: 'agents',
          title: 'Agents & runs',
          icon: Bot,
          content: <AutopilotPage project={project} />,
        },
        {
          id: 'cron-jobs',
          title: 'Cron jobs',
          icon: CalendarClock,
          content: <CronJobsSettings project={project} />,
        },
        {
          id: 'autofixer',
          title: 'Autofixer',
          icon: Sparkles,
          content: <AutofixerPage project={project} />,
        },
      ]
      break
    case 'integrations':
      sections = [
        {
          id: 'webhooks',
          title: 'Webhooks',
          icon: Webhook,
          content: <WebhooksSettings project={project} />,
        },
        {
          id: 'skills',
          title: 'Skills',
          icon: Puzzle,
          content: <SkillsSettings project={project} />,
        },
        {
          id: 'mcp-servers',
          title: 'MCP servers',
          icon: PlugZap,
          content: <McpServersSettings project={project} />,
        },
        {
          id: 'extensions',
          title: 'Extensions',
          icon: Boxes,
          content: <ProjectExtensionLinks project={project} />,
        },
      ]
      break
  }
  // A section can appear after the first render (the Advanced section waits
  // for the viewer's permissions), so its presence is part of the trigger.
  const activeSectionRendered = sections.some(
    (section) => section.id === activeSection
  )
  // Open the requested section and bring it into view. Runs again when the
  // query changes on the same page (e.g. ⌘K from one section to another).
  useEffect(() => {
    if (!activeSection || !activeSectionRendered) return
    const element = document.getElementById(settingsSectionDomId(activeSection))
    if (!(element instanceof HTMLDetailsElement)) return
    element.open = true
    element.scrollIntoView({ block: 'start' })
  }, [page, activeSection, activeSectionRendered])

  return (
    <div className="min-w-0 space-y-4">
      <h1 className="text-xl font-semibold tracking-tight">{titles[page]}</h1>
      {banner}
      {sections.map((section) => (
        <SettingsSection
          key={`${page}-${section.id}`}
          id={settingsSectionDomId(section.id)}
          title={section.title}
          icon={section.icon}
          defaultOpen={section.id === activeSection}
          className="scroll-mt-4"
        >
          {section.content}
        </SettingsSection>
      ))}
    </div>
  )
}

/**
 * Environment variables have one editor, on the project's own Environment
 * Variables page. This page keeps the entry point so nobody looking for them
 * under Settings hits a dead end.
 */
function EnvironmentVariablesLink({ project }: { project: ProjectResponse }) {
  return (
    <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
      <p className="max-w-[72ch] text-sm text-muted-foreground">
        Values injected into your app as environment variables, per environment.
        They are managed on the project&apos;s Environment Variables page, with
        credential checks and history.
      </p>
      <Button asChild variant="outline" className="shrink-0">
        <Link to={`/projects/${project.slug}/environment-variables`}>
          Open Environment Variables
        </Link>
      </Button>
    </div>
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
      Installed project extensions will appear here.
    </p>
  )
}
