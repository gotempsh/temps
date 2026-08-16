import { ProjectResponse } from '@/api/client'
import {
  AlarmClock,
  Bot,
  Boxes,
  ChevronRight,
  Flag,
  GitFork,
  Globe,
  KeyRound,
  Server,
  Settings2,
  Shield,
  SlidersHorizontal,
  Users,
  Wand2,
  Webhook,
  type LucideIcon,
} from 'lucide-react'
import { Link } from 'react-router'

interface SettingsItem {
  title: string
  description: string
  url: string
  icon: LucideIcon
}

interface SettingsGroup {
  label: string
  description: string
  items: SettingsItem[]
}

const settingsGroups: SettingsGroup[] = [
  {
    label: 'Environments',
    description: 'Runtime configuration for each deployment target.',
    items: [
      {
        title: 'Containers & environments',
        description: 'Inspect runtime health and switch environments.',
        url: 'environments',
        icon: Boxes,
      },
      {
        title: 'Environment settings',
        description: 'Resources, scaling, branch, network, and subdomain.',
        url: 'environments?view=settings',
        icon: SlidersHorizontal,
      },
      {
        title: 'Environment variables',
        description: 'Configure values available to deployments.',
        url: 'settings/environment-variables',
        icon: KeyRound,
      },
      {
        title: 'Domains',
        description: 'Attach and manage project domains.',
        url: 'settings/domains',
        icon: Globe,
      },
    ],
  },
  {
    label: 'Project',
    description: 'Defaults and access that apply across every environment.',
    items: [
      {
        title: 'General',
        description: 'Project identity and default deployment behavior.',
        url: 'settings/general',
        icon: Settings2,
      },
      {
        title: 'Secrets',
        description: 'Store encrypted project credentials.',
        url: 'settings/secrets',
        icon: KeyRound,
      },
      {
        title: 'Security',
        description: 'Headers and project security controls.',
        url: 'settings/security',
        icon: Shield,
      },
      {
        title: 'Access',
        description: 'Control who can view and change this project.',
        url: 'settings/access',
        icon: Users,
      },
    ],
  },
  {
    label: 'Delivery',
    description: 'How changes become running deployments.',
    items: [
      {
        title: 'Git repository',
        description: 'Repository connection and deployment automation.',
        url: 'settings/git',
        icon: GitFork,
      },
      {
        title: 'Build & deploy',
        description: 'Build commands, paths, ports, and deployment defaults.',
        url: 'settings/build',
        icon: Settings2,
      },
      {
        title: 'Feature flags',
        description: 'Release behavior independently from deployments.',
        url: 'flags',
        icon: Flag,
      },
      {
        title: 'Alert rules',
        description: 'Choose which errors trigger notifications.',
        url: 'errors/alert-rules',
        icon: AlarmClock,
      },
    ],
  },
  {
    label: 'Automation',
    description: 'Scheduled work and programmable project extensions.',
    items: [
      {
        title: 'Cron jobs',
        description: 'Run commands on a schedule.',
        url: 'settings/cron-jobs',
        icon: AlarmClock,
      },
      {
        title: 'Webhooks',
        description: 'Notify external systems about project events.',
        url: 'settings/webhooks',
        icon: Webhook,
      },
      {
        title: 'AI workflows',
        description: 'Automate project work with agents.',
        url: 'agents',
        icon: Bot,
      },
      {
        title: 'Skills',
        description: 'Give project agents reusable capabilities.',
        url: 'settings/skills',
        icon: Wand2,
      },
      {
        title: 'MCP servers',
        description: 'Connect project agents to external tools.',
        url: 'settings/mcp-servers',
        icon: Server,
      },
    ],
  },
]

export function ProjectSettingsOverview({
  project,
}: {
  project: ProjectResponse
}) {
  const hrefFor = (url: string) => `/projects/${project.slug}/${url}`

  return (
    <div className="mx-auto w-full max-w-6xl px-4 py-8 sm:px-6 lg:px-8">
      <div className="mb-8 max-w-2xl">
        <p className="text-xs font-medium uppercase tracking-[0.16em] text-muted-foreground">
          Configuration
        </p>
        <h2 className="mt-2 text-2xl font-semibold tracking-tight sm:text-3xl">
          Configure {project.name}
        </h2>
        <p className="mt-2 text-sm leading-6 text-muted-foreground">
          Project-wide defaults and environment-specific runtime controls,
          organized by the scope they affect.
        </p>
      </div>

      <div className="grid gap-x-10 gap-y-10 lg:grid-cols-2">
        {settingsGroups.map((group) => (
          <section
            key={group.label}
            aria-labelledby={`settings-${group.label}`}
          >
            <div className="mb-3">
              <h3
                id={`settings-${group.label}`}
                className="text-sm font-semibold text-foreground"
              >
                {group.label}
              </h3>
              <p className="mt-0.5 text-xs leading-5 text-muted-foreground">
                {group.description}
              </p>
            </div>
            <div className="overflow-hidden rounded-lg border bg-background">
              {group.items.map((item, index) => (
                <Link
                  key={item.url}
                  to={hrefFor(item.url)}
                  className={`group flex items-center gap-3 px-4 py-3.5 transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring ${
                    index > 0 ? 'border-t' : ''
                  }`}
                >
                  <div className="flex size-9 shrink-0 items-center justify-center rounded-md border bg-muted/30 text-muted-foreground transition-colors group-hover:text-foreground">
                    <item.icon className="size-4" aria-hidden="true" />
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="text-sm font-medium text-foreground">
                      {item.title}
                    </p>
                    <p className="mt-0.5 hidden truncate text-xs text-muted-foreground sm:block">
                      {item.description}
                    </p>
                  </div>
                  <ChevronRight
                    className="size-4 shrink-0 text-muted-foreground/60 transition-transform group-hover:translate-x-0.5 group-hover:text-foreground"
                    aria-hidden="true"
                  />
                </Link>
              ))}
            </div>
          </section>
        ))}
      </div>
    </div>
  )
}
