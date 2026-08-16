import {
  Activity,
  Bot,
  Box,
  Cloud,
  Database,
  DatabaseBackup,
  Folder,
  Gauge,
  GitBranch,
  Globe,
  Mail,
  Network,
  PackageOpen,
  Puzzle,
  Radar,
  Rocket,
  ScrollText,
  Settings,
  ShieldCheck,
  Sparkles,
  Workflow,
  type LucideIcon,
} from 'lucide-react'

export interface PlatformToolItem {
  title: string
  description: string
  url: string
  icon: LucideIcon
  keywords?: string[]
  featureKey?: string
}

export interface PlatformToolGroup {
  label: string
  description: string
  icon: LucideIcon
  items: PlatformToolItem[]
}

export const platformToolGroups: PlatformToolGroup[] = [
  {
    label: 'Applications and runtimes',
    description: 'Deploy applications and use isolated development runtimes.',
    icon: Rocket,
    items: [
      {
        title: 'Projects',
        description: 'Deploy and operate applications.',
        url: '/projects',
        icon: Folder,
        keywords: ['apps', 'deployments', 'sites'],
      },
      {
        title: 'Sandboxes',
        description: 'Run isolated development environments.',
        url: '/sandboxes',
        icon: Box,
        keywords: ['development', 'runtime'],
        featureKey: 'sandboxes-preview-environments',
      },
      {
        title: 'Git providers',
        description: 'Connect repositories and source providers.',
        url: '/git-providers',
        icon: GitBranch,
        keywords: ['github', 'gitlab', 'source'],
      },
    ],
  },
  {
    label: 'Data and delivery',
    description: 'Manage persistent services, backups, email, and domains.',
    icon: PackageOpen,
    items: [
      {
        title: 'Databases',
        description: 'Create and operate managed data services.',
        url: '/storage',
        icon: Database,
        keywords: ['postgres', 'mysql', 'redis', 'storage'],
      },
      {
        title: 'Backups',
        description: 'Schedule, inspect, and restore S3 backups.',
        url: '/backups',
        icon: DatabaseBackup,
        keywords: ['s3', 'restore', 'recovery'],
      },
      {
        title: 'Email',
        description: 'Configure transactional email delivery.',
        url: '/email',
        icon: Mail,
        keywords: ['smtp', 'transactional'],
      },
      {
        title: 'Domains',
        description: 'Attach domains to projects and services.',
        url: '/domains',
        icon: Globe,
        keywords: ['hostname', 'dns'],
      },
      {
        title: 'Certificates',
        description: 'Inspect and manage TLS certificates.',
        url: '/certificates',
        icon: ShieldCheck,
        keywords: ['tls', 'ssl', 'https'],
      },
      {
        title: 'DNS providers',
        description: 'Connect DNS automation providers.',
        url: '/dns-providers',
        icon: Cloud,
        keywords: ['cloudflare', 'dns'],
      },
    ],
  },
  {
    label: 'Observe',
    description: 'Understand platform health, traffic, and operator activity.',
    icon: Radar,
    items: [
      {
        title: 'Monitoring',
        description:
          'Review active alerts, resource health, and alarm history.',
        url: '/monitoring/alerts',
        icon: Gauge,
        keywords: ['health', 'alarms', 'resources'],
        featureKey: 'alerts-metric-alerts',
      },
      {
        title: 'Proxy',
        description: 'Inspect traffic and reverse-proxy performance.',
        url: '/proxy',
        icon: Activity,
        keywords: ['traffic', 'requests', 'metrics'],
      },
      {
        title: 'Proxy logs',
        description: 'Search requests handled by the platform proxy.',
        url: '/proxy-logs',
        icon: Network,
        keywords: ['requests', 'http', 'logs'],
      },
      {
        title: 'Audit logs',
        description: 'Review security-sensitive operator actions.',
        url: '/audit-logs',
        icon: ScrollText,
        keywords: ['activity', 'security', 'history'],
      },
    ],
  },
  {
    label: 'Automate',
    description: 'Configure AI-assisted and programmable platform workflows.',
    icon: Workflow,
    items: [
      {
        title: 'Connect AI harness',
        description:
          'Give Codex, Claude Code, Cursor, or another harness access to Temps.',
        url: '/setup/ai',
        icon: Sparkles,
        keywords: [
          'skill',
          'bunx',
          'api key',
          'codex',
          'claude',
          'cursor',
          'agent',
        ],
      },
      {
        title: 'Built-in AI',
        description: 'Manage providers, usage, console chat, and agent tools.',
        url: '/ai-gateway',
        icon: Sparkles,
        keywords: ['models', 'providers', 'chat', 'autofix', 'gateway'],
        featureKey: 'ai-gateway',
      },
      {
        title: 'AI workflows',
        description: 'Build reusable automated workflows.',
        url: '/ai-workflows',
        icon: Bot,
        keywords: ['automation', 'agents'],
        featureKey: 'ai-agents-workflows',
      },
      {
        title: 'Settings',
        description: 'Configure access, infrastructure, and security.',
        url: '/settings',
        icon: Settings,
        keywords: ['users', 'authentication', 'platform'],
      },
    ],
  },
]

export const extensionToolGroupIcon = Puzzle

export const platformToolShortcuts: PlatformToolItem[] = [
  {
    title: 'Create a project',
    description: 'Deploy an application from Git, an image, or files.',
    url: '/projects/new',
    icon: Folder,
  },
  {
    title: 'Add a database',
    description: 'Provision a persistent service.',
    url: '/storage/create',
    icon: Database,
  },
  {
    title: 'Configure a domain',
    description: 'Connect a hostname and TLS.',
    url: '/domains',
    icon: Globe,
  },
  {
    title: 'Inspect platform traffic',
    description: 'Open reverse-proxy traffic and performance.',
    url: '/proxy',
    icon: Activity,
  },
]
