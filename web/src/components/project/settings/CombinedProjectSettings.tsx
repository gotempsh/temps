// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
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
  let sections: { title: string; content: ReactNode }[]
  switch (page) {
    case 'general':
      sections = [
        {
          title: 'Project settings',
          content: <GeneralSettings project={project} refetch={refetch} />,
        },
        { title: 'Project setup', content: <ProjectSetup project={project} /> },
      ]
      break
    case 'delivery':
      sections = [
        {
          title: 'Source',
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
          content: <GitSettings project={project} refetch={refetch} />,
        },
        {
          title: 'Build',
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
          content: <ProjectFeatureFlags project={project} />,
        },
      ]
      break
    case 'variables':
      sections = [
        {
          title: 'Environment variables',
          content: <EnvironmentVariablesSettings project={project} />,
        },
        { title: 'Secrets', content: <SecretsSettings project={project} /> },
        {
          title: 'Deployment tokens',
          content: <DeploymentTokensSettings project={project} />,
        },
      ]
      break
    case 'automation':
      sections = [
        {
          title: 'Agents & runs',
          content: <AutopilotPage project={project} />,
        },
        { title: 'Cron jobs', content: <CronJobsSettings project={project} /> },
        { title: 'Autofixer', content: <AutofixerPage project={project} /> },
      ]
      break
    case 'integrations':
      sections = [
        { title: 'Webhooks', content: <WebhooksSettings project={project} /> },
        { title: 'Skills', content: <SkillsSettings project={project} /> },
        {
          title: 'MCP servers',
          content: <McpServersSettings project={project} />,
        },
        {
          title: 'Extensions',
          content: <ProjectExtensionLinks project={project} />,
        },
      ]
      break
  }
  return (
    <div className="min-w-0 space-y-4">
      <h1 className="text-xl font-semibold tracking-tight">{titles[page]}</h1>
      {sections.map((section, index) => (
        <SettingsSection
          key={`${page}-${section.title}`}
          title={section.title}
          initiallyOpen={index === 0}
        >
          {section.content}
        </SettingsSection>
      ))}
    </div>
  )
}

function SettingsSection({
  title,
  initiallyOpen,
  children,
}: {
  title: string
  initiallyOpen: boolean
  children: ReactNode
}) {
  const [visited, setVisited] = useState(initiallyOpen)
  return (
    <details
      open={initiallyOpen}
      onToggle={(event) => {
        if (event.currentTarget.open) setVisited(true)
      }}
      className="rounded-lg border bg-background"
    >
      <summary className="cursor-pointer rounded-lg px-4 py-3 text-sm font-medium hover:bg-muted/50 focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring">
        {title}
      </summary>
      {visited && <div className="min-w-0 border-t p-4">{children}</div>}
    </details>
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
