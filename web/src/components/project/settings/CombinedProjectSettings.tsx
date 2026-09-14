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
  PlugZap,
  Puzzle,
  Rocket,
  Settings2,
  Sparkles,
  Webhook,
} from 'lucide-react'
import type { ReactNode } from 'react'
import type { LucideIcon } from 'lucide-react'
import type { ProjectResponse } from '@/api/client'
import { usePageTitle } from '@/hooks/usePageTitle'
import { GeneralSettings } from './GeneralSettings'
import { GitSettings } from './GitSettings'
import { BuildDeploySettings } from './BuildDeploySettings'
import { EnvironmentVariablesSettings } from './EnvironmentVariablesSettings'
import { SecretsSettings } from './SecretsSettings'
import { DeploymentTokensSettings } from './DeploymentTokensSettings'
import { CronJobsSettings } from './CronJobsSettings'
import { WebhooksSettings } from './WebhooksSettings'
import { SkillsSettings } from './SkillsSettings'
import { McpServersSettings } from './McpServersSettings'
import { ProjectFeatureFlags } from '@/components/project/flags/ProjectFeatureFlags'
import { AutopilotPage } from '@/components/agents/AutopilotPage'
import { AutofixerPage } from '@/components/autofixer/AutofixerPage'
import { ProjectSetup } from '@/pages/ProjectSetup'
import { Link } from 'react-router'
import { usePluginsContext } from '@/contexts/PluginsContext'
import { useConsoleExtensions } from '@temps-sdk/console-kit'

export type CombinedSettingsPage =
  'general' | 'delivery' | 'variables' | 'automation' | 'integrations'
const titles: Record<CombinedSettingsPage, string> = {
  general: 'General',
  delivery: 'Build & deploy',
  variables: 'Variables & secrets',
  automation: 'Automation',
  integrations: 'Integrations',
}

/** Related forms share a URL. Disclosure sections reduce scrolling without another navigation level. */
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
  let sections: { title: string; icon: LucideIcon; content: ReactNode }[]
  switch (page) {
    case 'general':
      sections = [
        {
          title: 'Project settings',
          icon: Settings2,
          content: <GeneralSettings project={project} refetch={refetch} />,
        },
        {
          title: 'Project setup',
          icon: ListChecks,
          content: <ProjectSetup project={project} />,
        },
      ]
      break
    case 'delivery':
      sections = [
        {
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
          title: 'Repository',
          icon: GitBranch,
          content: <GitSettings project={project} refetch={refetch} />,
        },
        {
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
          title: 'Feature flags',
          icon: Flag,
          content: <ProjectFeatureFlags project={project} />,
        },
      ]
      break
    case 'variables':
      sections = [
        {
          title: 'Environment variables',
          icon: Braces,
          content: <EnvironmentVariablesSettings project={project} />,
        },
        {
          title: 'Secrets',
          icon: LockKeyhole,
          content: <SecretsSettings project={project} />,
        },
        {
          title: 'Deployment tokens',
          icon: KeyRound,
          content: <DeploymentTokensSettings project={project} />,
        },
      ]
      break
    case 'automation':
      sections = [
        {
          title: 'Agents & runs',
          icon: Bot,
          content: <AutopilotPage project={project} />,
        },
        {
          title: 'Cron jobs',
          icon: CalendarClock,
          content: <CronJobsSettings project={project} />,
        },
        {
          title: 'Autofixer',
          icon: Sparkles,
          content: <AutofixerPage project={project} />,
        },
      ]
      break
    case 'integrations':
      sections = [
        {
          title: 'Webhooks',
          icon: Webhook,
          content: <WebhooksSettings project={project} />,
        },
        {
          title: 'Skills',
          icon: Puzzle,
          content: <SkillsSettings project={project} />,
        },
        {
          title: 'MCP servers',
          icon: PlugZap,
          content: <McpServersSettings project={project} />,
        },
        {
          title: 'Extensions',
          icon: Boxes,
          content: <ProjectExtensionLinks project={project} />,
        },
      ]
      break
  }
  return (
    <div className="min-w-0 space-y-4">
      <h1 className="text-xl font-semibold tracking-tight">{titles[page]}</h1>
      {sections.map((section) => (
        <SettingsSection
          key={`${page}-${section.title}`}
          title={section.title}
          icon={section.icon}
        >
          {section.content}
        </SettingsSection>
      ))}
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
